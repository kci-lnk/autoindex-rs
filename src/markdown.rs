use std::io::Read;
use std::path::Path;

use ammonia::Builder;
use cap_std::fs::Dir;
use comrak::nodes::{AlertType, NodeValue};
use comrak::{Arena, Options, format_html, parse_document};
use lol_html::{RewriteStrSettings, element, rewrite_str};
use unicode_general_category::{GeneralCategory, get_general_category};

use crate::path_policy::{is_safe_visible_name, open_validated_regular};

const README_NAME: &str = "README.md";
const README_LIMIT: u64 = 1024 * 1024;

/// Reads and renders the exact `README.md` in `directory`.
///
/// Any read, encoding, size, or safety failure is deliberately treated as an
/// absent README so a malformed document never prevents directory browsing.
pub fn render_readme(root: &Dir, directory: &Path) -> Option<String> {
    let requested = directory.join(README_NAME);
    let (file, metadata) = open_validated_regular(root, &requested)?;
    if !metadata.is_file() || metadata.len() > README_LIMIT {
        return None;
    }

    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(README_LIMIT + 1).read_to_end(&mut bytes).ok()?;
    if bytes.len() as u64 > README_LIMIT {
        return None;
    }

    let markdown = String::from_utf8(bytes).ok()?;
    Some(render_markdown(&markdown))
}

pub fn render_markdown(markdown: &str) -> String {
    let mut options = Options::default();
    options.extension.strikethrough = true;
    options.extension.tagfilter = true;
    options.extension.table = true;
    options.extension.autolink = true;
    options.extension.tasklist = true;
    options.extension.alerts = true;

    let arena = Arena::new();
    let document = parse_document(&arena, markdown, &options);
    constrain_alerts_to_exact_top_level(&arena, document, markdown);
    let mut rendered = String::new();
    if format_html(document, &options, &mut rendered).is_err() {
        return String::new();
    }

    let mut sanitizer = Builder::default();
    sanitizer.add_generic_attributes(&["class"]);
    sanitizer.add_tags(&["input"]);
    sanitizer.add_tag_attributes("input", &["type", "checked", "disabled"]);
    sanitizer.link_rel(Some("nofollow noopener noreferrer"));
    let sanitized = sanitizer.clean(&rendered).to_string();

    rewrite_str(
        &sanitized,
        RewriteStrSettings::new()
            .append_element_content_handler(element!("img", |element| {
                let keep = element
                    .get_attribute("src")
                    .is_some_and(|source| is_safe_image_source(&source));
                if keep {
                    element.remove_attribute("srcset");
                    element.remove_attribute("sizes");
                    element.set_attribute("loading", "lazy")?;
                    element.set_attribute("referrerpolicy", "no-referrer")?;
                } else {
                    element.remove();
                }
                Ok(())
            }))
            .append_element_content_handler(element!("a", |element| {
                element.set_attribute("rel", "nofollow noopener noreferrer")?;
                if element
                    .get_attribute("href")
                    .is_some_and(|href| href.starts_with("https://") || href.starts_with("http://"))
                {
                    element.set_attribute("target", "_blank")?;
                } else {
                    element.remove_attribute("target");
                }
                Ok(())
            })),
    )
    .unwrap_or_default()
}

fn constrain_alerts_to_exact_top_level<'a>(
    arena: &'a Arena<'a>,
    document: comrak::nodes::Node<'a>,
    markdown: &str,
) {
    let source_lines: Vec<&str> = markdown.lines().collect();
    let alerts: Vec<_> = document
        .descendants()
        .filter_map(|node| {
            let data = node.data();
            let NodeValue::Alert(alert) = &data.value else {
                return None;
            };
            let marker = alert_marker(alert.alert_type);
            let source_is_exact = data
                .sourcepos
                .start
                .line
                .checked_sub(1)
                .and_then(|line| source_lines.get(line))
                .is_some_and(|line| line.trim_start() == format!("> {marker}"));
            let parent_is_document = node
                .parent()
                .is_some_and(|parent| matches!(parent.data().value, NodeValue::Document));
            (!parent_is_document || !source_is_exact).then(|| {
                let marker = alert
                    .title
                    .as_ref()
                    .map_or_else(|| marker.to_string(), |title| format!("{marker} {title}"));
                (node, marker)
            })
        })
        .collect();

    for (node, marker) in alerts {
        node.data_mut().value = NodeValue::BlockQuote;
        let text = arena.alloc(NodeValue::Text(marker.into()).into());
        if let Some(first) = node.first_child()
            && matches!(first.data().value, NodeValue::Paragraph)
        {
            let line_break = arena.alloc(NodeValue::SoftBreak.into());
            first.prepend(line_break);
            first.prepend(text);
        } else {
            let paragraph = arena.alloc(NodeValue::Paragraph.into());
            paragraph.append(text);
            node.prepend(paragraph);
        }
    }
}

const fn alert_marker(alert_type: AlertType) -> &'static str {
    match alert_type {
        AlertType::Note => "[!NOTE]",
        AlertType::Tip => "[!TIP]",
        AlertType::Important => "[!IMPORTANT]",
        AlertType::Warning => "[!WARNING]",
        AlertType::Caution => "[!CAUTION]",
    }
}

fn is_safe_image_source(source: &str) -> bool {
    let source = source.trim();
    if source.is_empty()
        || source.starts_with("//")
        || source.contains('\\')
        || source.chars().any(|character| {
            character.is_control()
                || matches!(get_general_category(character), GeneralCategory::Format)
        })
    {
        return false;
    }

    let raw_path = source.split(['?', '#']).next().unwrap_or_default();
    if has_forbidden_percent_encoding(raw_path) {
        return false;
    }

    let base =
        url::Url::parse("http://autoindex.invalid/current/").expect("static base URL is valid");
    let Ok(parsed) = base.join(source) else {
        return false;
    };
    if parsed.scheme() != base.scheme()
        || parsed.host_str() != base.host_str()
        || parsed.port_or_known_default() != base.port_or_known_default()
    {
        return false;
    }

    let Ok(decoded_path) = percent_encoding::percent_decode_str(parsed.path()).decode_utf8() else {
        return false;
    };
    decoded_path
        .split('/')
        .filter(|component| !component.is_empty())
        .all(|component| is_safe_visible_name(component) && !component.starts_with("__"))
}

fn has_forbidden_percent_encoding(source: &str) -> bool {
    let bytes = source.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'%' {
            index += 1;
            continue;
        }
        if index + 2 >= bytes.len() {
            return true;
        }
        let Some(high) = hex_value(bytes[index + 1]) else {
            return true;
        };
        let Some(low) = hex_value(bytes[index + 2]) else {
            return true;
        };
        let decoded = (high << 4) | low;
        if decoded == b'/' || decoded == b'\\' || decoded == b'%' || decoded.is_ascii_control() {
            return true;
        }
        index += 3;
    }
    false
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_gfm_and_top_level_alerts() {
        let html = render_markdown(
            "# Project\n\n~~gone~~\n\n- [x] shipped\n\n|A|B|\n|-|-|\n|1|2|\n\n> [!NOTE]\n> Note.\n\n> [!TIP]\n> Tip.\n\n> [!IMPORTANT]\n> Important.\n\n> [!WARNING]\n> Warning.\n\n> [!CAUTION]\n> Caution.\n",
        );
        assert!(html.contains("<table>"));
        assert!(html.contains("<del>gone</del>"));
        assert!(html.contains("type=\"checkbox\""), "{html}");
        for kind in ["note", "tip", "important", "warning", "caution"] {
            assert!(html.contains(&format!("markdown-alert-{kind}")), "{html}");
        }
    }

    #[test]
    fn nested_alert_marker_stays_plain() {
        let html = render_markdown("- > [!TIP]\n  > Nested\n");
        assert!(!html.contains("markdown-alert-tip"), "{html}");
        assert!(html.contains("[!TIP]"), "{html}");
    }

    #[test]
    fn alert_filter_does_not_rewrite_code_or_inline_text() {
        let html = render_markdown(
            "```text\n[!TIP] literal code\n```\n\nordinary [!NOTE] text\n\n> [!WARNING] Custom title\n> Plain quote.\n",
        );
        assert!(html.contains("[!TIP] literal code"), "{html}");
        assert!(html.contains("ordinary [!NOTE] text"), "{html}");
        assert!(html.contains("[!WARNING] Custom title"), "{html}");
        assert!(!html.contains("markdown-alert-warning"), "{html}");
        assert!(!html.contains("\\[!"), "{html}");
    }

    #[test]
    fn removes_active_content_and_external_images() {
        let html = render_markdown(
            "<script>alert(1)</script>\n\n![external](https://example.com/a.png)\n\n![local](./images/a.png?version=1#fragment)\n\n![root](/assets/a.png)\n\n[x](javascript:alert(1))\n\n[safe](https://example.com/)",
        );
        assert!(!html.contains("script"));
        assert!(!html.contains("https://example.com/a.png"));
        assert!(!html.contains("javascript:"));
        assert!(html.contains("./images/a.png?version=1#fragment"), "{html}");
        assert!(html.contains("/assets/a.png"), "{html}");
        assert!(
            html.contains("rel=\"nofollow noopener noreferrer\""),
            "{html}"
        );
        assert!(html.contains("target=\"_blank\""), "{html}");
    }
}

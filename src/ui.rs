// SPDX-License-Identifier: GPL-3.0-only

/// Calculates word and character count for any UTF-8 text string.
pub fn count_words_and_chars(text: &str) -> (usize, usize) {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return (0, 0);
    }
    let words = trimmed.split_whitespace().count();
    let chars = text.chars().count();
    (words, chars)
}

/// Extracts a clean preview snippet from note content.
/// If `query` is non-empty, finds the line matching query.
/// Otherwise returns the second line (after title) or first line.
pub fn get_preview_snippet(text: &str, query: &str) -> String {
    let query_lower = query.trim().to_lowercase();
    if !query_lower.is_empty() {
        for line in text.lines() {
            let trimmed = line.trim();
            if trimmed.to_lowercase().contains(&query_lower) {
                let cleaned = trimmed.trim_start_matches('#').trim();
                let chars: Vec<char> = cleaned.chars().collect();
                return if chars.len() > 42 {
                    let s: String = chars.into_iter().take(42).collect();
                    format!("{}…", s.trim_end())
                } else {
                    cleaned.to_string()
                };
            }
        }
    }

    let mut non_empty_lines = text
        .lines()
        .map(|l| l.trim().trim_start_matches('#').trim())
        .filter(|l| !l.is_empty());

    let _title_line = non_empty_lines.next();
    if let Some(body_line) = non_empty_lines.next() {
        let chars: Vec<char> = body_line.chars().collect();
        if chars.len() > 42 {
            let s: String = chars.into_iter().take(42).collect();
            format!("{}…", s.trim_end())
        } else {
            body_line.to_string()
        }
    } else {
        "…".to_string()
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;

    #[test]
    fn test_word_and_char_counter_empty() {
        assert_eq!(count_words_and_chars(""), (0, 0));
        assert_eq!(count_words_and_chars("   \n\t  "), (0, 0));
    }

    #[test]
    fn test_word_and_char_counter_unicode_and_multiline() {
        assert_eq!(count_words_and_chars("hello world"), (2, 11));
        assert_eq!(
            count_words_and_chars("Merhaba dünya! Şekerli çay."),
            (4, 27)
        );
        assert_eq!(
            count_words_and_chars("curl -X POST https://api.com/v1\nAuthorization: Bearer xyz"),
            (7, 57)
        );
    }

    #[test]
    fn test_get_preview_snippet_matching_query() {
        let text = "# Meeting\nDiscuss architecture\nAction item: buy milk";
        assert_eq!(get_preview_snippet(text, "milk"), "Action item: buy milk");
    }

    #[test]
    fn test_get_preview_snippet_empty_or_fallback() {
        let text = "# Only Title";
        assert_eq!(get_preview_snippet(text, ""), "…");

        let multiline = "# Title\nFirst line of body text here";
        assert_eq!(
            get_preview_snippet(multiline, ""),
            "First line of body text here"
        );
    }
}

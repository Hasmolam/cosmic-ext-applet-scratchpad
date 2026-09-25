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
}

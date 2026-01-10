use crate::models::EmailAddress;

pub fn parse_addresses(input: &str) -> Vec<EmailAddress> {
    input
        .split(',')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| {
            if let Some(start) = s.find('<')
                && let Some(end) = s.find('>')
            {
                let name = s[..start].trim();
                let email = s[start + 1..end].trim();
                return EmailAddress {
                    name: if name.is_empty() {
                        None
                    } else {
                        Some(name.to_string())
                    },
                    email: email.to_string(),
                };
            }
            EmailAddress {
                name: None,
                email: s.to_string(),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_single_email() {
        let result = parse_addresses("test@example.com");
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].email, "test@example.com");
        assert!(result[0].name.is_none());
    }

    #[test]
    fn test_parse_multiple_emails() {
        let result = parse_addresses("a@example.com, b@example.com");
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].email, "a@example.com");
        assert_eq!(result[1].email, "b@example.com");
    }

    #[test]
    fn test_parse_email_with_name() {
        let result = parse_addresses("John Doe <john@example.com>");
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].email, "john@example.com");
        assert_eq!(result[0].name, Some("John Doe".to_string()));
    }

    #[test]
    fn test_parse_mixed_formats() {
        let result = parse_addresses("plain@example.com, Named User <named@example.com>");
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].email, "plain@example.com");
        assert!(result[0].name.is_none());
        assert_eq!(result[1].email, "named@example.com");
        assert_eq!(result[1].name, Some("Named User".to_string()));
    }

    #[test]
    fn test_parse_empty_string() {
        let result = parse_addresses("");
        assert!(result.is_empty());
    }

    #[test]
    fn test_parse_whitespace_handling() {
        let result = parse_addresses("  spaced@example.com  ,  other@example.com  ");
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].email, "spaced@example.com");
        assert_eq!(result[1].email, "other@example.com");
    }

    #[test]
    fn test_parse_angle_brackets_no_name() {
        let result = parse_addresses("<bare@example.com>");
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].email, "bare@example.com");
        assert!(result[0].name.is_none());
    }
}

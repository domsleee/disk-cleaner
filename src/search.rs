//! Name and on-disk size filters, compiled once per search edit.

#[derive(Debug)]
pub struct Query {
    terms: Vec<Term>,
}

#[derive(Debug)]
enum Term {
    Name(String),
    Glob(Vec<char>),
    Size(Comparison, u64),
}

#[derive(Debug)]
enum Comparison {
    Less,
    LessEqual,
    Equal,
    GreaterEqual,
    Greater,
}

impl Query {
    pub fn parse(input: &str) -> Result<Self, String> {
        let mut terms = Vec::new();
        let mut chars = input.chars().peekable();
        while let Some(c) = chars.next() {
            if c.is_whitespace() {
                continue;
            }
            let quoted = c == '"';
            let mut word = String::new();
            if quoted {
                let mut closed = false;
                for c in chars.by_ref() {
                    if c == '"' {
                        closed = true;
                        break;
                    }
                    word.push(c);
                }
                if !closed {
                    return Err("Close the quoted file name with a double quote.".into());
                }
                if chars.peek().is_some_and(|c| !c.is_whitespace()) {
                    return Err("Separate search terms with spaces.".into());
                }
            } else {
                word.push(c);
                while chars.peek().is_some_and(|c| !c.is_whitespace()) {
                    word.push(chars.next().unwrap());
                }
            }
            let term = if !quoted && word.starts_with(['<', '>', '=']) {
                let (comparison, value) = if let Some(v) = word.strip_prefix(">=") {
                    (Comparison::GreaterEqual, v)
                } else if let Some(v) = word.strip_prefix("<=") {
                    (Comparison::LessEqual, v)
                } else if let Some(v) = word.strip_prefix('>') {
                    (Comparison::Greater, v)
                } else if let Some(v) = word.strip_prefix('<') {
                    (Comparison::Less, v)
                } else {
                    (Comparison::Equal, &word[1..])
                };
                Term::Size(
                    comparison,
                    parse_size(value).ok_or_else(|| {
                        format!("Invalid size: {word}. Try >1g, >=500m, or <=2.5g.")
                    })?,
                )
            } else if !quoted && word.contains(['*', '?']) {
                Term::Glob(word.chars().collect())
            } else {
                Term::Name(word)
            };
            terms.push(term);
        }
        Ok(Self { terms })
    }

    pub fn matches(&self, name: &str, size: u64) -> bool {
        self.terms.iter().all(|term| match term {
            Term::Name(needle) => {
                needle.is_empty()
                    || name
                        .as_bytes()
                        .windows(needle.len())
                        .any(|window| window.eq_ignore_ascii_case(needle.as_bytes()))
            }
            Term::Glob(pattern) => glob_matches(pattern, name),
            Term::Size(comparison, bytes) => match comparison {
                Comparison::Less => size < *bytes,
                Comparison::LessEqual => size <= *bytes,
                Comparison::Equal => size == *bytes,
                Comparison::GreaterEqual => size >= *bytes,
                Comparison::Greater => size > *bytes,
            },
        })
    }
}

fn parse_size(input: &str) -> Option<u64> {
    let suffix_at = input
        .find(|c: char| !c.is_ascii_digit() && c != '.')
        .unwrap_or(input.len());
    let (number, suffix) = input.split_at(suffix_at);
    let multiplier: u128 = match suffix.to_ascii_lowercase().as_str() {
        "" | "b" => 1,
        "k" | "kb" => 1_000,
        "m" | "mb" => 1_000_000,
        "g" | "gb" => 1_000_000_000,
        "t" | "tb" => 1_000_000_000_000,
        "kib" => 1 << 10,
        "mib" => 1 << 20,
        "gib" => 1 << 30,
        "tib" => 1 << 40,
        _ => return None,
    };
    let (whole, fraction) = number.split_once('.').unwrap_or((number, ""));
    if whole.is_empty()
        || !whole.bytes().all(|c| c.is_ascii_digit())
        || !fraction.bytes().all(|c| c.is_ascii_digit())
        || (number.contains('.') && fraction.is_empty())
    {
        return None;
    }
    let whole = whole.parse::<u128>().ok()?.checked_mul(multiplier)?;
    let fractional = if fraction.is_empty() {
        0
    } else {
        let divisor = 10u128.checked_pow(fraction.len().try_into().ok()?)?;
        let numerator = fraction.parse::<u128>().ok()?.checked_mul(multiplier)?;
        // Reject fractional bytes instead of rounding comparison boundaries.
        if numerator % divisor != 0 {
            return None;
        }
        numerator / divisor
    };
    whole.checked_add(fractional)?.try_into().ok()
}

// Greedy wildcard matching with no recursion or per-file allocation. `?`
// consumes a Unicode character, while `*` consumes zero or more characters.
fn glob_matches(pattern: &[char], name: &str) -> bool {
    let mut remaining = name.chars();
    let mut index = 0;
    let mut star = None;
    let mut retry = remaining.clone();
    while let Some(c) = remaining.clone().next() {
        if pattern.get(index) == Some(&'*') {
            star = Some(index);
            index += 1;
            retry = remaining.clone();
        } else if pattern
            .get(index)
            .is_some_and(|p| *p == '?' || p.eq_ignore_ascii_case(&c))
        {
            remaining.next();
            index += 1;
        } else if let Some(star_index) = star {
            retry.next();
            remaining = retry.clone();
            index = star_index + 1;
        } else {
            return false;
        }
    }
    pattern[index..].iter().all(|c| *c == '*')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_wildcards_and_combined_sizes() {
        let query = Query::parse("*.zip backup >1g <=2GiB").unwrap();
        assert!(query.matches("Backup-2026.ZIP", 1_500_000_000));
        assert!(!query.matches("Backup.zip.old", 1_500_000_000));
        assert!(!query.matches("other.zip", 1_500_000_000));
        assert!(!query.matches("backup.zip", 1_000_000_000));
        assert!(!query.matches("backup.zip", 3_000_000_000));
        assert!(
            Query::parse("\"summer holiday\" >=1.5m")
                .unwrap()
                .matches("Summer Holiday.mp4", 1_500_000)
        );
        assert!(Query::parse("\"=notes\"").unwrap().matches("=notes.txt", 0));
    }

    #[test]
    fn wildcards_match_whole_names_and_unicode_characters() {
        for (pattern, name, expected) in [
            ("*", "", true),
            ("a?c", "aéc", true),
            ("a?c", "ac", false),
            ("*.zip", "file.zip.old", false),
            ("*ab*cd", "xxabyyabzzcd", true),
            ("a**b", "ab", true),
            ("*a?", "a", false),
            ("a*b*c", "abbb", false),
        ] {
            assert_eq!(
                Query::parse(pattern).unwrap().matches(name, 0),
                expected,
                "{pattern}: {name}"
            );
        }
    }

    #[test]
    fn exact_size_boundaries_and_units() {
        assert!(
            Query::parse("=18446744073709551615")
                .unwrap()
                .matches("f", u64::MAX)
        );
        assert!(Query::parse(">=1KiB <2k").unwrap().matches("f", 1024));
        assert!(!Query::parse("<1k").unwrap().matches("f", 1000));
        assert_eq!(parse_size("1.25GB"), Some(1_250_000_000));
        assert_eq!(parse_size("0"), Some(0));
        assert!(Query::parse("  ").unwrap().matches("anything", 0));
    }

    #[test]
    fn invalid_input_is_reported() {
        for input in [
            ">",
            "> 1g",
            ">=oops",
            "=1XB",
            "<1.2.3g",
            "=1.",
            "=0.5",
            "=-1",
            "=18446744073709551616",
            "\"unfinished",
            "\"name\"extra",
        ] {
            assert!(Query::parse(input).is_err(), "{input}");
        }
    }
}

//! Filename sanitization for user-supplied strings being used as
//! filesystem paths. Strips path separators, the usual Windows/POSIX
//! illegal characters, leading dots, and trims whitespace.

/// Returns a filename safe to use on both POSIX and Windows filesystems.
/// Strips path separators and the Windows-illegal set, drops control
/// characters, collapses runs of whitespace into a single `_`, and trims
/// leading/trailing dots and whitespace. Returns "untitled" for inputs
/// that would otherwise produce an empty string.
pub fn sanitize_filename(name: &str) -> String {
    // Step 1: replace illegal chars.
    let mut out = String::with_capacity(name.len());
    for ch in name.chars() {
        match ch {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => out.push('_'),
            c if c.is_control() => {} // drop control chars
            c => out.push(c),
        }
    }
    // Step 2: collapse runs of whitespace into a single '_'.
    let mut collapsed = String::with_capacity(out.len());
    let mut prev_ws = false;
    for ch in out.chars() {
        if ch.is_whitespace() {
            if !prev_ws {
                collapsed.push('_');
            }
            prev_ws = true;
        } else {
            collapsed.push(ch);
            prev_ws = false;
        }
    }
    // Step 3: strip a *single* leading and trailing run of dots and
    // underscores so "..", ".", "  .." all fall through, but
    // "..etc.." becomes "etc" rather than the empty string.
    let trimmed = collapsed
        .trim_start_matches(|c: char| c == '.' || c == '_' || c.is_whitespace())
        .trim_end_matches(|c: char| c == '.' || c == '_' || c.is_whitespace());
    if trimmed.is_empty() {
        "untitled".into()
    } else {
        trimmed.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_path_separators() {
        assert_eq!(sanitize_filename("../etc/passwd"), "etc_passwd");
        assert_eq!(sanitize_filename("a/b\\c"), "a_b_c");
    }

    #[test]
    fn drops_control_and_windows_illegal() {
        // Trailing `_` is trimmed because the only reason it's there is
        // that `?` got replaced; without the trim we'd ship dangling
        // underscores on operators' filesystems.
        assert_eq!(sanitize_filename("foo:bar*?"), "foo_bar");
    }

    #[test]
    fn falls_back_for_empty() {
        assert_eq!(sanitize_filename("   ..  "), "untitled");
        assert_eq!(sanitize_filename("..."), "untitled");
    }

    #[test]
    fn collapses_whitespace_runs() {
        assert_eq!(sanitize_filename("foo  bar"), "foo_bar");
    }
}

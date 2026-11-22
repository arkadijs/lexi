//! Turning a GLSL compiler log into readable, source-annotated lines. Platform independent so it is
//! unit-tested on the desktop; the Android-only renderer logs what it returns.

/// Extract the 1-based source line from a driver message such as `ERROR: 0:42: ...` or `0(42) : error`.
pub fn error_line(entry: &str) -> Option<usize> {
    let digits = |s: &str| -> Option<usize> {
        let d: String = s.chars().take_while(char::is_ascii_digit).collect();
        d.parse().ok()
    };
    if let Some(i) = entry.find("0:") {
        if let Some(n) = digits(&entry[i + 2..]) {
            return Some(n);
        }
    }
    if let Some(i) = entry.find("0(") {
        return digits(&entry[i + 2..]);
    }
    None
}

/// The driver log followed, for every line it mentions, by that source line with one line of context
/// either side; the offending line is marked `>>`.
pub fn annotate(name: &str, src: &str, log: &str) -> Vec<String> {
    let lines: Vec<&str> = src.lines().collect();
    let mut out = vec![format!("shader {name} failed:\n{log}")];
    for entry in log.lines() {
        if let Some(n) = error_line(entry) {
            let from = n.saturating_sub(1).max(1);
            for i in from..=(n + 1).min(lines.len()) {
                let mark = if i == n { ">>" } else { "  " };
                out.push(format!("{name}:{i:>4} {mark} {}", lines[i - 1]));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_line_in_common_driver_messages() {
        assert_eq!(
            error_line("ERROR: 0:42: 'x' : undeclared identifier"),
            Some(42)
        );
        assert_eq!(
            error_line("0(17) : error C1008: undefined variable"),
            Some(17)
        );
        assert_eq!(error_line("something without a location"), None);
    }

    #[test]
    fn annotates_with_source_context() {
        let src = "a\nb\nc\nd";
        let out = annotate("t", src, "ERROR: 0:3: boom");
        assert!(out[0].contains("boom"));
        assert_eq!(out[1..], ["t:   2    b", "t:   3 >> c", "t:   4    d"]);
        // a line number past the end of the source is clamped, not a panic
        assert_eq!(annotate("t", src, "0:99: x").len(), 1);
    }
}

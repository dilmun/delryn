//! Best-effort LaTeX → Unicode for math, which EPUBs usually ship as
//! `<img alt="$$ … $$">` (the PNG can't render in a terminal). This won't
//! typeset matrices, but it makes inline math and simple expressions readable
//! and strips the `$$`/`\displaystyle`/`&` noise. Real math layout is out of
//! scope for a TUI.

/// Does this image alt look like math — LaTeX or MathML? (EPUBs ship math as an
/// `<img>` whose alt holds the source, in either notation.)
pub fn is_math(alt: &str) -> bool {
    let s = alt.trim();
    s.starts_with("$$")
        || s.starts_with('$')
        || s.starts_with("\\(")
        || s.starts_with("\\[")
        || s.contains("\\begin{")
        || s.contains("\\displaystyle")
        || is_mathml(s)
}

/// Does this string carry MathML markup (often serialised into an image alt by
/// OOXML/DOCX → EPUB converters)?
pub fn is_mathml(s: &str) -> bool {
    s.contains("<math") || s.contains("mml:math") || s.contains("MathML")
}

pub fn latex_to_unicode(raw: &str) -> String {
    let mut s = strip_delims(raw.trim());
    s = s
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&nbsp;", " ");
    s = collapse_double_backslashes(&s);

    // Structural commands → removed or spaced.
    for kw in [
        "\\displaystyle",
        "\\textstyle",
        "\\scriptstyle",
        "\\left",
        "\\right",
    ] {
        s = s.replace(kw, " ");
    }
    s = remove_envs(&s);
    s = unwrap_cmds(
        &s,
        &[
            "text",
            "textbf",
            "textit",
            "mathbf",
            "mathrm",
            "mathit",
            "mathsf",
            "mathcal",
            "mathbb",
            "boldsymbol",
            "mbox",
            "operatorname",
        ],
    );
    s = s.replace("{,}", ","); // protected thousands separator
    s = s.replace("\\\\", "\n"); // row break
    s = s.replace('&', " "); // column separator
    for (pat, rep) in [
        ("\\quad", "  "),
        ("\\qquad", "    "),
        ("\\,", " "),
        ("\\;", " "),
        ("\\:", " "),
        ("\\!", ""),
    ] {
        s = s.replace(pat, rep);
    }
    s = fracs(&s);
    s = replace_symbols(&s);
    s = scripts(&s);
    s = s.replace(['{', '}'], "");
    s = drop_backslashes(&s);
    collapse_ws(&s)
}

fn strip_delims(s: &str) -> String {
    let s = s.trim();
    for (open, close) in [("$$", "$$"), ("\\[", "\\]"), ("\\(", "\\)"), ("$", "$")] {
        if s.len() >= open.len() + close.len() && s.starts_with(open) && s.ends_with(close) {
            return s[open.len()..s.len() - close.len()].trim().to_string();
        }
    }
    s.to_string()
}

/// `\\times` (doubly-escaped command) → `\times`, but leave `\\ ` (row break).
fn collapse_double_backslashes(s: &str) -> String {
    let ch: Vec<char> = s.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < ch.len() {
        if ch[i] == '\\' && i + 2 < ch.len() && ch[i + 1] == '\\' && ch[i + 2].is_ascii_alphabetic()
        {
            out.push('\\');
            i += 2;
        } else {
            out.push(ch[i]);
            i += 1;
        }
    }
    out
}

/// Remove `\begin{...}` / `\end{...}` and their (up to two) brace arguments.
fn remove_envs(input: &str) -> String {
    let mut s = input.to_string();
    for kw in ["\\begin", "\\end"] {
        while let Some(pos) = s.find(kw) {
            let after = pos + kw.len();
            let rest: Vec<char> = s[after..].chars().collect();
            let mut idx = 0;
            for _ in 0..2 {
                while idx < rest.len() && rest[idx].is_whitespace() {
                    idx += 1;
                }
                if idx < rest.len() && rest[idx] == '{' {
                    let mut depth = 0;
                    while idx < rest.len() {
                        match rest[idx] {
                            '{' => depth += 1,
                            '}' => {
                                depth -= 1;
                                if depth == 0 {
                                    idx += 1;
                                    break;
                                }
                            }
                            _ => {}
                        }
                        idx += 1;
                    }
                } else {
                    break;
                }
            }
            let consumed: String = rest[..idx].iter().collect();
            let end = after + consumed.len();
            s.replace_range(pos..end, "");
        }
    }
    s
}

/// Replace `\cmd{inner}` (or `\cmd inner` / `\cmd x`) with `inner` for the given
/// font/text commands. Tolerates whitespace and brace-less single-token args.
fn unwrap_cmds(input: &str, cmds: &[&str]) -> String {
    let mut s = input.to_string();
    'outer: loop {
        for cmd in cmds {
            let needle = format!("\\{cmd}");
            let mut from = 0;
            while let Some(rel) = s[from..].find(&needle) {
                let pos = from + rel;
                let after = pos + needle.len();
                // Whole command only (e.g. don't let \text swallow \textbf).
                if s[after..]
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_ascii_alphabetic())
                {
                    from = after;
                    continue;
                }
                // Skip whitespace to the argument.
                let mut arg = after;
                while let Some(c) = s[arg..].chars().next() {
                    if c.is_whitespace() {
                        arg += c.len_utf8();
                    } else {
                        break;
                    }
                }
                if s[arg..].starts_with('{') {
                    if let Some((inner, close)) = take_group(&s, arg) {
                        s.replace_range(pos..close, &inner);
                        continue 'outer;
                    }
                } else if let Some(c) = s[arg..].chars().next()
                    && c.is_alphanumeric()
                {
                    s.replace_range(pos..arg + c.len_utf8(), &c.to_string());
                    continue 'outer;
                }
                from = after;
            }
        }
        break;
    }
    s
}

fn fracs(input: &str) -> String {
    let mut s = input.to_string();
    while let Some(pos) = s.find("\\frac{") {
        let Some((num, after_num)) = take_group(&s, pos + 5) else {
            break;
        };
        // expect a second group immediately after
        if s[after_num..].starts_with('{')
            && let Some((den, after_den)) = take_group(&s, after_num)
        {
            s.replace_range(pos..after_den, &format!("{num}/{den}"));
            continue;
        }
        break;
    }
    s
}

/// Given the byte index of an opening `{`, return (inner, index-after-`}`).
fn take_group(s: &str, brace: usize) -> Option<(String, usize)> {
    let bytes = s.as_bytes();
    if bytes.get(brace) != Some(&b'{') {
        return None;
    }
    let mut depth = 0;
    let mut inner = String::new();
    for (off, c) in s[brace..].char_indices() {
        match c {
            '{' => {
                depth += 1;
                if depth > 1 {
                    inner.push(c);
                }
            }
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some((inner, brace + off + 1));
                }
                inner.push(c);
            }
            _ => inner.push(c),
        }
    }
    None
}

fn replace_symbols(input: &str) -> String {
    let mut pairs: Vec<(&str, &str)> = SYMBOLS.to_vec();
    pairs.sort_by_key(|(p, _)| std::cmp::Reverse(p.len())); // longest first
    let mut s = input.to_string();
    for (pat, rep) in pairs {
        if s.contains(pat) {
            s = s.replace(pat, rep);
        }
    }
    s
}

fn scripts(input: &str) -> String {
    let ch: Vec<char> = input.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < ch.len() {
        let c = ch[i];
        if (c == '_' || c == '^') && i + 1 < ch.len() {
            let (arg, adv) = if ch[i + 1] == '{' {
                let mut depth = 1;
                let mut j = i + 2;
                let mut a = String::new();
                while j < ch.len() && depth > 0 {
                    match ch[j] {
                        '{' => depth += 1,
                        '}' => {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                        }
                        _ => {}
                    }
                    a.push(ch[j]);
                    j += 1;
                }
                (a, j + 1 - i)
            } else {
                (ch[i + 1].to_string(), 2)
            };
            match convert_script(&arg, c == '_') {
                Some(u) => out.push_str(&u),
                None => {
                    out.push(c);
                    out.push_str(&arg);
                }
            }
            i += adv;
        } else {
            out.push(c);
            i += 1;
        }
    }
    out
}

fn convert_script(arg: &str, sub: bool) -> Option<String> {
    let mut out = String::new();
    for c in arg.chars() {
        if c.is_whitespace() {
            out.push(c);
            continue;
        }
        let mapped = if sub { subscript(c) } else { superscript(c) };
        out.push(mapped?);
    }
    Some(out)
}

fn subscript(c: char) -> Option<char> {
    Some(match c {
        '0'..='9' => ['₀', '₁', '₂', '₃', '₄', '₅', '₆', '₇', '₈', '₉'][c as usize - '0' as usize],
        '+' => '₊',
        '-' => '₋',
        '=' => '₌',
        '(' => '₍',
        ')' => '₎',
        'a' => 'ₐ',
        'e' => 'ₑ',
        'h' => 'ₕ',
        'i' => 'ᵢ',
        'j' => 'ⱼ',
        'k' => 'ₖ',
        'l' => 'ₗ',
        'm' => 'ₘ',
        'n' => 'ₙ',
        'o' => 'ₒ',
        'p' => 'ₚ',
        'r' => 'ᵣ',
        's' => 'ₛ',
        't' => 'ₜ',
        'u' => 'ᵤ',
        'v' => 'ᵥ',
        'x' => 'ₓ',
        _ => return None,
    })
}

fn superscript(c: char) -> Option<char> {
    Some(match c {
        '0' => '⁰',
        '1' => '¹',
        '2' => '²',
        '3' => '³',
        '4' => '⁴',
        '5' => '⁵',
        '6' => '⁶',
        '7' => '⁷',
        '8' => '⁸',
        '9' => '⁹',
        '+' => '⁺',
        '-' => '⁻',
        '=' => '⁼',
        '(' => '⁽',
        ')' => '⁾',
        'n' => 'ⁿ',
        'i' => 'ⁱ',
        'T' => 'ᵀ',
        _ => return None,
    })
}

fn drop_backslashes(s: &str) -> String {
    s.chars().filter(|&c| c != '\\').collect()
}

fn collapse_ws(s: &str) -> String {
    s.lines()
        .map(|line| line.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

const SYMBOLS: &[(&str, &str)] = &[
    ("\\times", "×"),
    ("\\cdot", "·"),
    ("\\div", "÷"),
    ("\\pm", "±"),
    ("\\mp", "∓"),
    ("\\leq", "≤"),
    ("\\le", "≤"),
    ("\\geq", "≥"),
    ("\\ge", "≥"),
    ("\\neq", "≠"),
    ("\\ne", "≠"),
    ("\\approx", "≈"),
    ("\\equiv", "≡"),
    ("\\sim", "∼"),
    ("\\propto", "∝"),
    ("\\infty", "∞"),
    ("\\partial", "∂"),
    ("\\nabla", "∇"),
    ("\\sum", "∑"),
    ("\\prod", "∏"),
    ("\\int", "∫"),
    ("\\sqrt", "√"),
    ("\\angle", "∠"),
    ("\\perp", "⊥"),
    ("\\parallel", "∥"),
    ("\\rightarrow", "→"),
    ("\\to", "→"),
    ("\\leftarrow", "←"),
    ("\\Rightarrow", "⇒"),
    ("\\Leftarrow", "⇐"),
    ("\\leftrightarrow", "↔"),
    ("\\mapsto", "↦"),
    ("\\ldots", "…"),
    ("\\cdots", "…"),
    ("\\dots", "…"),
    ("\\vdots", "⋮"),
    ("\\ddots", "⋱"),
    ("\\in", "∈"),
    ("\\notin", "∉"),
    ("\\subseteq", "⊆"),
    ("\\subset", "⊂"),
    ("\\cup", "∪"),
    ("\\cap", "∩"),
    ("\\emptyset", "∅"),
    ("\\forall", "∀"),
    ("\\exists", "∃"),
    ("\\langle", "⟨"),
    ("\\rangle", "⟩"),
    ("\\alpha", "α"),
    ("\\beta", "β"),
    ("\\gamma", "γ"),
    ("\\delta", "δ"),
    ("\\varepsilon", "ε"),
    ("\\epsilon", "ε"),
    ("\\zeta", "ζ"),
    ("\\eta", "η"),
    ("\\theta", "θ"),
    ("\\iota", "ι"),
    ("\\kappa", "κ"),
    ("\\lambda", "λ"),
    ("\\mu", "μ"),
    ("\\nu", "ν"),
    ("\\xi", "ξ"),
    ("\\pi", "π"),
    ("\\rho", "ρ"),
    ("\\sigma", "σ"),
    ("\\tau", "τ"),
    ("\\upsilon", "υ"),
    ("\\varphi", "φ"),
    ("\\phi", "φ"),
    ("\\chi", "χ"),
    ("\\psi", "ψ"),
    ("\\omega", "ω"),
    ("\\Gamma", "Γ"),
    ("\\Delta", "Δ"),
    ("\\Theta", "Θ"),
    ("\\Lambda", "Λ"),
    ("\\Xi", "Ξ"),
    ("\\Pi", "Π"),
    ("\\Sigma", "Σ"),
    ("\\Phi", "Φ"),
    ("\\Psi", "Ψ"),
    ("\\Omega", "Ω"),
];

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
    let mut s = strip_delimiters(raw.trim());
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
            // Accents / over-under decorations: keep the base, drop the decoration
            // (a terminal can't stack a bar/hat), so `\overline{pq}` → `pq`.
            "overline",
            "underline",
            "widehat",
            "widetilde",
            "overbrace",
            "underbrace",
            "overrightarrow",
            "overleftarrow",
            "hat",
            "bar",
            "vec",
            "tilde",
            "dot",
            "ddot",
            "check",
            "acute",
            "grave",
            "breve",
        ],
    );
    // Spacing commands with a dimension (`\kern0.125em`, `\hspace{2pt}`) and
    // delimiter-size / bookkeeping noise (`\big`, `\limits`, `\nonumber`) — removed
    // word-boundary-aware so `\bigcap` / `\overline` aren't corrupted.
    s = strip_tex_noise(&s);
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
    s = replace_over(&s); // TeX infix fraction `{a \over b}` → `a / b`
    s = fracs(&s);
    s = replace_symbols(&s);
    s = scripts(&s);
    s = s.replace(['{', '}'], "");
    // Any command left unmapped (a rare macro, `\BigVert`, a custom `\newcommand`)
    // is dropped whole rather than leaking its name as literal text.
    s = strip_leftover_commands(&s);
    collapse_ws(&s)
}

/// Strip the outer math delimiters (`$$…$$`, `\[…\]`, `\(…\)`, `$…$`) from a math
/// source, returning the bare body. Public so the parser can hand RaTeX clean
/// LaTeX (the renderer wants the body, not the delimiters).
pub fn strip_delimiters(s: &str) -> String {
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

/// [`SYMBOLS`] pre-sorted longest-pattern-first (so `\alpha` matches before a
/// shorter prefix), built once. `replace_symbols` runs per math span during
/// parsing, so re-cloning and re-sorting the constant table every call was pure
/// repeated work.
static SYMBOLS_BY_LEN: std::sync::LazyLock<Vec<(&'static str, &'static str)>> =
    std::sync::LazyLock::new(|| {
        let mut pairs = SYMBOLS.to_vec();
        pairs.sort_by_key(|(p, _)| std::cmp::Reverse(p.len()));
        pairs
    });

fn replace_symbols(input: &str) -> String {
    let mut s = input.to_string();
    for (pat, rep) in SYMBOLS_BY_LEN.iter() {
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
            out.push_str(&render_script(&arg, c == '_'));
            i += adv;
        } else {
            out.push(c);
            i += 1;
        }
    }
    out
}

/// Render a script argument: Unicode super/subscript when every (non-space)
/// character maps, else a readable parenthesised fallback (`^(n+1)`), never raw
/// `^{…}` braces.
fn render_script(arg: &str, sub: bool) -> String {
    let tight: String = arg.split_whitespace().collect();
    let mapped = if sub {
        subscript_str(&tight)
    } else {
        superscript_str(&tight)
    };
    match mapped {
        Some(u) => u,
        None if tight.chars().count() <= 1 => {
            format!("{}{tight}", if sub { '_' } else { '^' })
        }
        None => format!("{}({tight})", if sub { '_' } else { '^' }),
    }
}

/// Map a string to Unicode subscripts, or `None` if any character has no
/// subscript form. Whitespace is dropped.
pub fn subscript_str(s: &str) -> Option<String> {
    let t: String = s.chars().filter(|c| !c.is_whitespace()).collect();
    if t.is_empty() {
        return None;
    }
    t.chars().map(subscript).collect()
}

/// Map a string to Unicode superscripts, or `None` if any character has no
/// superscript form. Whitespace is dropped.
pub fn superscript_str(s: &str) -> Option<String> {
    let t: String = s.chars().filter(|c| !c.is_whitespace()).collect();
    if t.is_empty() {
        return None;
    }
    t.chars().map(superscript).collect()
}

fn subscript(c: char) -> Option<char> {
    Some(match c {
        '0'..='9' => ['₀', '₁', '₂', '₃', '₄', '₅', '₆', '₇', '₈', '₉'][c as usize - '0' as usize],
        '+' => '₊',
        '-' | '−' => '₋',
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
        '-' | '−' => '⁻',
        '=' => '⁼',
        '(' => '⁽',
        ')' => '⁾',
        'a' => 'ᵃ',
        'b' => 'ᵇ',
        'c' => 'ᶜ',
        'd' => 'ᵈ',
        'e' => 'ᵉ',
        'f' => 'ᶠ',
        'g' => 'ᵍ',
        'h' => 'ʰ',
        'i' => 'ⁱ',
        'j' => 'ʲ',
        'k' => 'ᵏ',
        'l' => 'ˡ',
        'm' => 'ᵐ',
        'n' => 'ⁿ',
        'o' => 'ᵒ',
        'p' => 'ᵖ',
        'r' => 'ʳ',
        's' => 'ˢ',
        't' => 'ᵗ',
        'u' => 'ᵘ',
        'v' => 'ᵛ',
        'w' => 'ʷ',
        'x' => 'ˣ',
        'y' => 'ʸ',
        'z' => 'ᶻ',
        'A' => 'ᴬ',
        'B' => 'ᴮ',
        'D' => 'ᴰ',
        'E' => 'ᴱ',
        'G' => 'ᴳ',
        'H' => 'ᴴ',
        'I' => 'ᴵ',
        'J' => 'ᴶ',
        'K' => 'ᴷ',
        'L' => 'ᴸ',
        'M' => 'ᴹ',
        'N' => 'ᴺ',
        'O' => 'ᴼ',
        'P' => 'ᴾ',
        'R' => 'ᴿ',
        'T' => 'ᵀ',
        'U' => 'ᵁ',
        'V' => 'ⱽ',
        'W' => 'ᵂ',
        _ => return None,
    })
}

/// Drop any leftover `\command` (letters after a backslash) entirely — an unmapped
/// symbol or macro vanishes instead of leaking its bare name (`\mid` never becomes
/// "mid"). A backslash before a non-letter (`\{`, `\_`, `\%`) keeps the escaped
/// character. Runs last, after symbol replacement, so only unknowns remain.
fn strip_leftover_commands(s: &str) -> String {
    let ch: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < ch.len() {
        if ch[i] == '\\' {
            let mut j = i + 1;
            if j < ch.len() && ch[j].is_ascii_alphabetic() {
                while j < ch.len() && ch[j].is_ascii_alphabetic() {
                    j += 1; // drop the whole command name
                }
            } else if j < ch.len() {
                out.push(ch[j]); // escaped literal: keep the character
                j += 1;
            }
            i = j;
        } else {
            out.push(ch[i]);
            i += 1;
        }
    }
    out
}

/// Commands taking a dimension (`\kern0.125em`) — removed with their argument.
const DIM_CMDS: &[&str] = &[
    "kern", "mkern", "hskip", "mskip", "hspace", "vspace", "raise", "lower",
];

/// Argument-less noise: delimiter sizers and script/number bookkeeping — removed
/// (their operand, e.g. the `(` after `\big`, stays).
const NOISE_CMDS: &[&str] = &[
    "bigl",
    "bigr",
    "biggl",
    "biggr",
    "Bigl",
    "Bigr",
    "Biggl",
    "Biggr",
    "bigm",
    "Bigm",
    "biggm",
    "Biggm",
    "big",
    "Big",
    "bigg",
    "Bigg",
    "limits",
    "nolimits",
    "displaystyle",
    "textstyle",
    "scriptstyle",
    "scriptscriptstyle",
    "nonumber",
    "notag",
    "mathstrut",
    "strut",
    "mathord",
    "mathbin",
    "mathrel",
    "mathop",
];

/// Strip spacing-with-dimension and delimiter-size / bookkeeping commands,
/// word-boundary-aware: only a whole `\name` matches, so `\bigcap` and `\overline`
/// (real symbols) survive to [`replace_symbols`]. A [`DIM_CMDS`] command also
/// consumes a following `{group}` or a bare dimension (`0.125em`, `2pt`, `3mu`).
fn strip_tex_noise(s: &str) -> String {
    let ch: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < ch.len() {
        if ch[i] != '\\' || i + 1 >= ch.len() || !ch[i + 1].is_ascii_alphabetic() {
            out.push(ch[i]);
            i += 1;
            continue;
        }
        let start = i + 1;
        let mut j = start;
        while j < ch.len() && ch[j].is_ascii_alphabetic() {
            j += 1;
        }
        let name: String = ch[start..j].iter().collect();
        if DIM_CMDS.contains(&name.as_str()) {
            i = skip_dimension(&ch, j);
        } else if NOISE_CMDS.contains(&name.as_str()) {
            i = j;
        } else {
            out.extend(&ch[i..j]); // keep the command for later passes
            i = j;
        }
    }
    out
}

/// From index `k`, skip optional whitespace then a dimension argument — a
/// `{…}` brace group, or a `[+-]?digits[.digits]?<unit-letters>` run — and return
/// the index after it. Used to remove a spacing command's operand.
fn skip_dimension(ch: &[char], k: usize) -> usize {
    let mut i = k;
    while i < ch.len() && ch[i] == ' ' {
        i += 1;
    }
    if i < ch.len() && ch[i] == '{' {
        let mut depth = 0;
        while i < ch.len() {
            match ch[i] {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    i += 1;
                    if depth == 0 {
                        return i;
                    }
                    continue;
                }
                _ => {}
            }
            i += 1;
        }
        return i;
    }
    // A bare dimension: sign, digits, optional fraction, then unit letters.
    if i < ch.len() && (ch[i] == '+' || ch[i] == '-') {
        i += 1;
    }
    let num_start = i;
    while i < ch.len() && (ch[i].is_ascii_digit() || ch[i] == '.') {
        i += 1;
    }
    if i == num_start {
        return k; // not a dimension after all — leave it
    }
    while i < ch.len() && ch[i].is_ascii_alphabetic() {
        i += 1; // unit (em, ex, pt, mu, px, …)
    }
    i
}

/// Replace the TeX infix fraction command `\over` (word-boundary only, so
/// `\overline`/`\overbrace` are untouched) with a readable ` / `.
fn replace_over(s: &str) -> String {
    let ch: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < ch.len() {
        if ch[i] == '\\' && ch[i + 1..].starts_with(&['o', 'v', 'e', 'r']) {
            let after = i + 5;
            if after >= ch.len() || !ch[after].is_ascii_alphabetic() {
                out.push_str(" / ");
                i = after;
                continue;
            }
        }
        out.push(ch[i]);
        i += 1;
    }
    out
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
    // Vertical bars / norms (cardinality, absolute value, matrix norms).
    ("\\lvert", "|"),
    ("\\rvert", "|"),
    ("\\vert", "|"),
    ("\\mid", "|"),
    ("\\nmid", "∤"),
    ("\\lVert", "‖"),
    ("\\rVert", "‖"),
    ("\\Vert", "‖"),
    ("\\|", "‖"),
    ("\\parallel", "∥"),
    // Big operators (∑/∏ already below; these round out the family).
    ("\\bigcap", "⋂"),
    ("\\bigcup", "⋃"),
    ("\\bigsqcup", "⨆"),
    ("\\bigvee", "⋁"),
    ("\\bigwedge", "⋀"),
    ("\\bigoplus", "⨁"),
    ("\\bigotimes", "⨂"),
    ("\\bigodot", "⨀"),
    ("\\biguplus", "⨄"),
    // Upright ("var") capital Greek used by some LaTeX math fonts.
    ("\\varGamma", "Γ"),
    ("\\varDelta", "Δ"),
    ("\\varTheta", "Θ"),
    ("\\varLambda", "Λ"),
    ("\\varXi", "Ξ"),
    ("\\varPi", "Π"),
    ("\\varSigma", "Σ"),
    ("\\varUpsilon", "Υ"),
    ("\\varPhi", "Φ"),
    ("\\varPsi", "Ψ"),
    ("\\varOmega", "Ω"),
    // Lowercase Greek variant glyphs.
    ("\\varepsilon", "ε"),
    ("\\vartheta", "ϑ"),
    ("\\varpi", "ϖ"),
    ("\\varrho", "ϱ"),
    ("\\varsigma", "ς"),
    ("\\varphi", "φ"),
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

#[cfg(test)]
mod tests {
    use super::{latex_to_unicode, strip_delimiters};

    /// The Unicode fallback never leaks LaTeX macro *names* — a command it can't
    /// map to a glyph is dropped, not spelled out. Regression for the real-book
    /// garbling (`\mid`→"mid", `\kern0.125em`→"kern0.125em", `\BigVert`→"BigVert").
    #[test]
    fn fallback_never_leaks_command_names() {
        let cases = [
            (r"\mid A \cap B \mid", "| A ∩ B |"),
            (r"(1-\alpha)\kern0.125em \mid B \mid", "(1-α) | B |"),
            (r"\sqrt{|A| \Big\Vert |B|}", "√|A| ‖ |B|"),
            (r"{P(A\cap B) \over P(B)}", "P(A∩ B) / P(B)"),
            (r"\varDelta x", "Δ x"),
            (r"\overline{pq}", "pq"),
            (r"\bigcap_{i} A_i", "⋂ᵢ Aᵢ"),
        ];
        for (tex, want) in cases {
            let got = latex_to_unicode(tex);
            assert_eq!(got, want, "latex_to_unicode({tex:?})");
            // No bare LaTeX command name survives as prose.
            for leak in ["mid", "kern", "Vert", "over", "varDelta", "big", "frac"] {
                assert!(!got.contains(leak), "{tex:?} leaked {leak:?}: {got:?}");
            }
        }
    }

    /// A spacing command's dimension argument is consumed whole (`\hspace{2pt}`,
    /// `\kern-3mu`), and a word-boundary keeps `\bigcap`/`\overline` intact.
    #[test]
    fn strips_dimensions_and_respects_word_boundaries() {
        assert_eq!(latex_to_unicode(r"a\hspace{2pt}b"), "ab");
        assert_eq!(latex_to_unicode(r"a\kern-3mu b"), "a b");
        // `\big(` drops the sizer but keeps the delimiter it sized.
        assert_eq!(latex_to_unicode(r"\big( x \big)"), "( x )");
    }

    /// Delimiter stripping exposes the bare body for the renderer.
    #[test]
    fn strips_outer_delimiters() {
        assert_eq!(strip_delimiters(r"$$ \frac12 $$"), r"\frac12");
        assert_eq!(strip_delimiters(r"\( x^2 \)"), "x^2");
        assert_eq!(strip_delimiters(r"\[ a+b \]"), "a+b");
        assert_eq!(strip_delimiters("$y$"), "y");
        assert_eq!(strip_delimiters("no delims"), "no delims");
    }
}

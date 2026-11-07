//! The chemistry helper: molar masses, mass composition and mole / gram conversion, exact and
//! on the spot ("what is the molar mass of H2SO4", "how many moles are 36 grams of water").
//!
//! A formula is parsed properly — element symbols, counts, nested brackets, a leading
//! coefficient and hydrates (`CuSO4*5H2O`) — against the standard atomic weights, so the answer
//! is arithmetic and never a language model's guess. Substances Sir names in words ("water",
//! "Kochsalz") and formulas the speech recogniser spells out ("H two S O four") are understood
//! too.

use serde_json::{json, Value};

/// Standard atomic weights (g/mol) of the elements school chemistry meets.
const ELEMENTS: &[(&str, &str, f64)] = &[
    ("H", "hydrogen", 1.008),
    ("He", "helium", 4.0026),
    ("Li", "lithium", 6.94),
    ("Be", "beryllium", 9.0122),
    ("B", "boron", 10.81),
    ("C", "carbon", 12.011),
    ("N", "nitrogen", 14.007),
    ("O", "oxygen", 15.999),
    ("F", "fluorine", 18.998),
    ("Ne", "neon", 20.180),
    ("Na", "sodium", 22.990),
    ("Mg", "magnesium", 24.305),
    ("Al", "aluminium", 26.982),
    ("Si", "silicon", 28.085),
    ("P", "phosphorus", 30.974),
    ("S", "sulfur", 32.06),
    ("Cl", "chlorine", 35.45),
    ("Ar", "argon", 39.948),
    ("K", "potassium", 39.098),
    ("Ca", "calcium", 40.078),
    ("Ti", "titanium", 47.867),
    ("Cr", "chromium", 51.996),
    ("Mn", "manganese", 54.938),
    ("Fe", "iron", 55.845),
    ("Co", "cobalt", 58.933),
    ("Ni", "nickel", 58.693),
    ("Cu", "copper", 63.546),
    ("Zn", "zinc", 65.38),
    ("Br", "bromine", 79.904),
    ("Kr", "krypton", 83.798),
    ("Sr", "strontium", 87.62),
    ("Ag", "silver", 107.87),
    ("Sn", "tin", 118.71),
    ("I", "iodine", 126.90),
    ("Xe", "xenon", 131.29),
    ("Ba", "barium", 137.33),
    ("W", "tungsten", 183.84),
    ("Pt", "platinum", 195.08),
    ("Au", "gold", 196.97),
    ("Hg", "mercury", 200.59),
    ("Pb", "lead", 207.2),
    ("U", "uranium", 238.03),
];

/// Substances by name (English and German) and their formulas.
const SUBSTANCES: &[(&[&str], &str)] = &[
    (&["water", "wasser"], "H2O"),
    (&["table salt", "salt", "sodium chloride", "kochsalz", "natriumchlorid"], "NaCl"),
    (&["carbon dioxide", "kohlenstoffdioxid", "kohlendioxid"], "CO2"),
    (&["carbon monoxide", "kohlenstoffmonoxid"], "CO"),
    (&["glucose", "traubenzucker"], "C6H12O6"),
    (&["sucrose", "sugar", "zucker", "saccharose"], "C12H22O11"),
    (&["sulfuric acid", "sulphuric acid", "schwefelsäure"], "H2SO4"),
    (&["hydrochloric acid", "hydrogen chloride", "salzsäure", "chlorwasserstoff"], "HCl"),
    (&["nitric acid", "salpetersäure"], "HNO3"),
    (&["ammonia", "ammoniak"], "NH3"),
    (&["methane", "methan"], "CH4"),
    (&["ethanol", "alcohol", "alkohol"], "C2H5OH"),
    (&["acetic acid", "essigsäure"], "CH3COOH"),
    (&["sodium hydroxide", "natriumhydroxid", "natronlauge"], "NaOH"),
    (&["calcium carbonate", "limestone", "calciumcarbonat", "kalk"], "CaCO3"),
    (&["oxygen", "sauerstoff"], "O2"),
    (&["hydrogen", "wasserstoff"], "H2"),
    (&["nitrogen", "stickstoff"], "N2"),
    (&["hydrogen peroxide", "wasserstoffperoxid"], "H2O2"),
    (&["copper sulfate", "kupfersulfat"], "CuSO4"),
];

fn weight(symbol: &str) -> Option<f64> {
    ELEMENTS.iter().find(|(s, _, _)| *s == symbol).map(|(_, _, w)| *w)
}

/// The atoms of a formula as (symbol, count), in the order they first appear.
pub fn atoms(formula: &str) -> Result<Vec<(String, u32)>, String> {
    let compact: String = formula.chars().filter(|c| !c.is_whitespace()).collect();
    if compact.is_empty() {
        return Err("Which formula, Sir?".to_string());
    }
    let mut total: Vec<(String, u32)> = Vec::new();
    // A hydrate is its parts added up: CuSO4*5H2O.
    for part in compact.split(['*', '·', '.']) {
        let chars: Vec<char> = part.chars().collect();
        // A leading coefficient multiplies the whole part ("5H2O").
        let digits = chars.iter().take_while(|c| c.is_ascii_digit()).count();
        let coefficient: u32 = if digits == 0 { 1 } else { chars[..digits].iter().collect::<String>().parse().map_err(|_| format!("'{part}' has a coefficient I can't read."))? };
        let mut at = digits;
        let group = parse_group(&chars, &mut at, 0)?;
        if at != chars.len() {
            return Err(format!("'{formula}' has a bracket too many."));
        }
        for (symbol, count) in group {
            add(&mut total, &symbol, count.saturating_mul(coefficient));
        }
    }
    if total.is_empty() {
        return Err(format!("'{formula}' has no elements in it."));
    }
    Ok(total)
}

fn add(total: &mut Vec<(String, u32)>, symbol: &str, count: u32) {
    match total.iter_mut().find(|(s, _)| s == symbol) {
        Some((_, n)) => *n = n.saturating_add(count),
        None => total.push((symbol.to_string(), count)),
    }
}

fn number(chars: &[char], at: &mut usize) -> u32 {
    let start = *at;
    while *at < chars.len() && chars[*at].is_ascii_digit() {
        *at += 1;
    }
    chars[start..*at].iter().collect::<String>().parse().unwrap_or(1).max(1)
}

/// The atoms from `at` up to the bracket that closes this group (or the end at depth 0).
fn parse_group(chars: &[char], at: &mut usize, depth: usize) -> Result<Vec<(String, u32)>, String> {
    if depth > 8 {
        return Err("That formula is nested deeper than any molecule, Sir.".to_string());
    }
    let mut group: Vec<(String, u32)> = Vec::new();
    while *at < chars.len() {
        let c = chars[*at];
        match c {
            '(' | '[' => {
                *at += 1;
                let inner = parse_group(chars, at, depth + 1)?;
                if *at >= chars.len() || !matches!(chars[*at], ')' | ']') {
                    return Err("A bracket in that formula never closes.".to_string());
                }
                *at += 1;
                let times = number(chars, at);
                for (symbol, count) in inner {
                    add(&mut group, &symbol, count.saturating_mul(times));
                }
            }
            ')' | ']' => {
                if depth == 0 {
                    return Err("That formula closes a bracket it never opened.".to_string());
                }
                return Ok(group);
            }
            c if c.is_ascii_uppercase() => {
                // The longest symbol that is an element: "Cl" before "C" + "l".
                let two: Option<String> = chars.get(*at + 1).filter(|n| n.is_ascii_lowercase()).map(|n| [c, *n].iter().collect());
                let symbol = match two.filter(|s| weight(s).is_some()) {
                    Some(s) => s,
                    None if weight(&c.to_string()).is_some() => c.to_string(),
                    None => return Err(format!("I don't know an element '{}'.", chars[*at..(*at + 2).min(chars.len())].iter().collect::<String>())),
                };
                *at += symbol.len();
                let count = number(chars, at);
                add(&mut group, &symbol, count);
            }
            other => return Err(format!("'{other}' does not belong in a formula.")),
        }
    }
    Ok(group)
}

/// The molar mass of a formula in g/mol, and each element's (symbol, count, share of the mass).
pub fn molar_mass(formula: &str) -> Result<(f64, Vec<(String, u32, f64)>), String> {
    let atoms = atoms(formula)?;
    let total: f64 = atoms.iter().map(|(s, n)| weight(s).unwrap_or(0.0) * *n as f64).sum();
    let parts = atoms.into_iter().map(|(s, n)| {
        let share = weight(&s).unwrap_or(0.0) * n as f64 / total;
        (s, n, share)
    });
    Ok((total, parts.collect()))
}

/// A formula from what Sir said: a name ("water", "Kochsalz"), a formula as written ("h2so4",
/// "NaCl") or one spelt out by the speech recogniser ("H two S O four", "C O 2").
pub fn formula_from(spoken: &str) -> Option<String> {
    let raw = spoken.trim().trim_end_matches(['?', '.', '!', ',']).trim();
    if raw.is_empty() {
        return None;
    }
    let lower = raw.to_lowercase();
    if let Some((_, formula)) = SUBSTANCES.iter().find(|(names, _)| names.contains(&lower.as_str())) {
        return Some(formula.to_string());
    }
    // An element by name ("iron", "copper") is its symbol.
    if let Some((symbol, _, _)) = ELEMENTS.iter().find(|(_, name, _)| *name == lower) {
        return Some(symbol.to_string());
    }
    // Spelt out: single letters, element symbols and number words.
    let words: Vec<&str> = raw.split_whitespace().collect();
    let digit = |w: &str| match w.to_lowercase().as_str() {
        "one" | "eins" => Some("1"),
        "two" | "to" | "too" | "zwei" => Some("2"),
        "three" | "drei" => Some("3"),
        "four" | "for" | "vier" => Some("4"),
        "five" | "fünf" => Some("5"),
        "six" | "sechs" => Some("6"),
        "seven" | "sieben" => Some("7"),
        "eight" | "acht" => Some("8"),
        "nine" | "neun" => Some("9"),
        "ten" | "zehn" => Some("10"),
        "twelve" | "zwölf" => Some("12"),
        _ => None,
    };
    let joined: String = if words.len() > 1 {
        let mut out = String::new();
        for w in &words {
            match digit(w) {
                Some(d) if !out.is_empty() => out.push_str(d),
                _ if w.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '(' | ')')) && w.len() <= 3 => {
                    // "h" → "H", "na" → "Na", "cl" → "Cl".
                    let mut cs = w.chars();
                    let first = cs.next()?;
                    out.push(first.to_ascii_uppercase());
                    out.extend(cs.map(|c| c.to_ascii_lowercase()));
                }
                _ => return None,
            }
        }
        out
    } else {
        raw.to_string()
    };
    // As written, in whatever case: try it as it is, then with the capitals a formula has.
    let candidates = [joined.clone(), capitalised(&joined)];
    candidates.into_iter().find(|f| f.chars().any(|c| c.is_ascii_uppercase()) && atoms(f).is_ok())
}

/// How common an element is in the formulas Sir meets (5 = H, C, N, O … 1 = rare).
fn commonness(symbol: &str) -> u8 {
    match symbol {
        "H" | "C" | "N" | "O" => 5,
        "S" | "Cl" | "Na" | "K" | "Ca" | "P" => 4,
        "Fe" | "Cu" | "Mg" | "Al" | "Zn" | "Ag" => 3,
        "F" | "Br" | "I" | "Si" | "Li" | "Mn" | "Ba" | "Pb" => 2,
        _ => 1,
    }
}

/// "h2so4" → "H2SO4", "nacl" → "NaCl", "caco3" → "CaCO3": a formula typed without capitals
/// reads as the split into element symbols whose rarest element is the most common (then the
/// fewest symbols) — "co" is carbon and oxygen, not cobalt; "cu" is copper, not carbon and
/// uranium. A text that is no formula comes back unchanged.
fn capitalised(formula: &str) -> String {
    let chars: Vec<char> = formula.chars().map(|c| c.to_ascii_lowercase()).collect();
    // The best reading of chars[i..]: (rarest element's commonness, symbols used, text).
    fn best(chars: &[char], i: usize, memo: &mut Vec<Option<Option<(u8, usize, String)>>>) -> Option<(u8, usize, String)> {
        if i == chars.len() {
            return Some((u8::MAX, 0, String::new()));
        }
        if let Some(known) = &memo[i] {
            return known.clone();
        }
        let c = chars[i];
        let found = if c.is_ascii_alphabetic() {
            let mut options: Vec<(u8, usize, String)> = Vec::new();
            for len in [1usize, 2] {
                if i + len > chars.len() || !chars[i..i + len].iter().all(|c| c.is_ascii_alphabetic()) {
                    continue;
                }
                let symbol: String = chars[i..i + len].iter().enumerate().map(|(k, c)| if k == 0 { c.to_ascii_uppercase() } else { *c }).collect();
                if weight(&symbol).is_none() {
                    continue;
                }
                if let Some((rarest, used, rest)) = best(chars, i + len, memo) {
                    options.push((rarest.min(commonness(&symbol)), used + 1, format!("{symbol}{rest}")));
                }
            }
            options.into_iter().max_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(&a.1)))
        } else {
            best(chars, i + 1, memo).map(|(rarest, used, rest)| (rarest, used, format!("{c}{rest}")))
        };
        memo[i] = Some(found.clone());
        found
    }
    let mut memo = vec![None; chars.len() + 1];
    best(&chars, 0, &mut memo).map_or_else(|| formula.to_string(), |(_, _, text)| text)
}

fn trim(value: f64) -> String {
    let s = format!("{value:.3}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

pub fn tool_definition() -> Value {
    json!({"type": "function", "function": {
        "name": "chemistry_helper",
        "description": "Exact chemistry arithmetic: action molar_mass (formula: 'H2SO4', 'CuSO4*5H2O' or a name such as 'water'), composition (each element's share of the mass), moles (formula + grams → mol), grams (formula + moles → g). Use it for every molar-mass or mole question instead of calculating yourself.",
        "parameters": {"type": "object", "properties": {
            "action": {"type": "string", "enum": ["molar_mass", "composition", "moles", "grams"]},
            "formula": {"type": "string", "description": "A formula or a substance's name."},
            "grams": {"type": "number"},
            "moles": {"type": "number"}
        }, "required": ["formula"]}
    }})
}

/// The `chemistry_helper` tool: the display block and one spoken line.
pub fn run_tool(args: &Value) -> Result<String, String> {
    let named = crate::features::str_arg(args, &["formula", "substance", "name"]).ok_or("Which substance, Sir?")?;
    let formula = formula_from(named).ok_or_else(|| format!("I can't read '{named}' as a formula, Sir."))?;
    let (mass, parts) = molar_mass(&formula)?;
    let action = crate::features::str_arg(args, &["action"]).unwrap_or("molar_mass").to_lowercase();
    let composition: Vec<String> = parts.iter().map(|(s, n, share)| format!("{s} x{n}: {:.2} %", share * 100.0)).collect();
    let grams = crate::features::num_arg(args, &["grams", "mass", "g"]);
    let moles = crate::features::num_arg(args, &["moles", "mol", "amount"]);
    match (action.as_str(), grams, moles) {
        ("moles", Some(g), _) | ("molar_mass", Some(g), None) => {
            let n = g / mass;
            Ok(crate::features::spoken(format!("{formula}: {} g / {} g/mol = {} mol", trim(g), trim(mass), trim(n)), format!("{} grams of {formula} are {} moles, Sir.", trim(g), trim(n))))
        }
        ("grams", _, Some(n)) | ("molar_mass", None, Some(n)) => {
            let g = n * mass;
            Ok(crate::features::spoken(format!("{formula}: {} mol x {} g/mol = {} g", trim(n), trim(mass), trim(g)), format!("{} moles of {formula} weigh {} grams, Sir.", trim(n), trim(g))))
        }
        ("moles", None, _) => Err("How many grams, Sir?".to_string()),
        ("grams", _, None) => Err("How many moles, Sir?".to_string()),
        ("composition", _, _) => {
            let heaviest = parts.iter().max_by(|a, b| a.2.total_cmp(&b.2)).map(|(s, _, share)| format!("{s} makes up {:.1} percent of the mass", share * 100.0)).unwrap_or_default();
            Ok(crate::features::spoken(format!("{formula}: {} g/mol\n{}", trim(mass), composition.join("\n")), format!("{formula}: {heaviest}, Sir.")))
        }
        _ => Ok(crate::features::spoken(format!("{formula}: {} g/mol\n{}", trim(mass), composition.join("\n")), format!("The molar mass of {formula} is {} grams per mole, Sir.", trim(mass)))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mass(formula: &str) -> f64 {
        molar_mass(formula).unwrap_or_else(|e| panic!("{formula}: {e}")).0
    }

    #[test]
    fn molar_masses_match_the_tables() {
        for (formula, expected) in [("H2O", 18.015), ("NaCl", 58.44), ("H2SO4", 98.072), ("C6H12O6", 180.156), ("CO2", 44.009), ("Ca(OH)2", 74.092), ("Al2(SO4)3", 342.132), ("CuSO4*5H2O", 249.677), ("2H2O", 36.03), ("Fe2O3", 159.687), ("K4[Fe(CN)6]", 368.346)] {
            assert!((mass(formula) - expected).abs() < 0.02, "{formula}: {} vs {expected}", mass(formula));
        }
        // Counts add up through brackets and hydrates.
        assert_eq!(atoms("Al2(SO4)3").unwrap(), vec![("Al".to_string(), 2), ("S".to_string(), 3), ("O".to_string(), 12)]);
        assert_eq!(atoms("CuSO4*5H2O").unwrap(), vec![("Cu".to_string(), 1), ("S".to_string(), 1), ("O".to_string(), 9), ("H".to_string(), 10)]);
        // The shares of the mass sum to one.
        let (_, parts) = molar_mass("C2H5OH").unwrap();
        assert!((parts.iter().map(|p| p.2).sum::<f64>() - 1.0).abs() < 1e-9);
    }

    #[test]
    fn broken_formulas_are_refused_with_a_reason() {
        for bad in ["", "H2O)", "(H2O", "Xx2", "H2O!", "h2o"] {
            assert!(atoms(bad).is_err(), "{bad}");
        }
        assert!(atoms(&"(".repeat(20)).is_err());
    }

    #[test]
    fn names_and_spoken_formulas_become_formulas() {
        assert_eq!(formula_from("water").as_deref(), Some("H2O"));
        assert_eq!(formula_from("Kochsalz?").as_deref(), Some("NaCl"));
        assert_eq!(formula_from("H two S O four").as_deref(), Some("H2SO4"));
        assert_eq!(formula_from("C O 2").as_deref(), Some("CO2"));
        assert_eq!(formula_from("h2so4").as_deref(), Some("H2SO4"));
        assert_eq!(formula_from("nacl").as_deref(), Some("NaCl"));
        assert_eq!(formula_from("NaCl").as_deref(), Some("NaCl"));
        assert_eq!(formula_from("co2").as_deref(), Some("CO2"));
        assert_eq!(formula_from("the weather in Berlin"), None);
        assert_eq!(formula_from(""), None);
    }

    #[test]
    fn the_tool_answers_mass_composition_and_conversions() {
        let said = run_tool(&json!({"formula": "H2SO4"})).unwrap();
        assert!(said.contains("98.072 g/mol") && said.contains("Spoken: The molar mass of H2SO4 is 98.072 grams per mole, Sir."), "{said}");
        let said = run_tool(&json!({"action": "moles", "formula": "water", "grams": 36})).unwrap();
        assert!(said.contains("= 1.998 mol") && said.contains("36 grams of H2O are 1.998 moles"), "{said}");
        let said = run_tool(&json!({"action": "grams", "formula": "NaCl", "moles": 2})).unwrap();
        assert!(said.contains("= 116.88 g"), "{said}");
        let said = run_tool(&json!({"action": "composition", "formula": "CO2"})).unwrap();
        assert!(said.contains("O x2: 72.71 %") && said.contains("O makes up 72.7 percent"), "{said}");
        assert!(run_tool(&json!({"action": "moles", "formula": "H2O"})).is_err());
        assert!(run_tool(&json!({"formula": "unobtainium"})).is_err());
        assert!(run_tool(&json!({})).is_err());
    }
}

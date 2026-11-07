//! The formula sheet: the formulas of school maths, physics and chemistry, looked up by what
//! Sir calls them in English or German ("the formula for kinetic energy", "pq-Formel",
//! "Ohmsches Gesetz") and shown as a block with every symbol explained.
//!
//! Each entry is written out once, by hand, and returned verbatim: a formula is a fact, and a
//! small language model reciting one from memory gets a sign or an exponent wrong often enough
//! to matter in an exam.

use serde_json::{json, Value};

/// One formula: what it is called, how it is written, how it is read aloud and what its
/// symbols mean.
pub struct Formula {
    pub subject: &'static str,
    pub name: &'static str,
    /// Every name and keyword it answers to, lower case (English and German).
    pub keys: &'static [&'static str],
    pub written: &'static str,
    pub spoken: &'static str,
    pub symbols: &'static str,
}

const fn f(subject: &'static str, name: &'static str, keys: &'static [&'static str], written: &'static str, spoken: &'static str, symbols: &'static str) -> Formula {
    Formula { subject, name, keys, written, spoken, symbols }
}

pub const SHEET: &[Formula] = &[
    // ---- Maths ----
    f("maths", "Quadratic formula", &["quadratic formula", "quadratic equation", "abc formula", "mitternachtsformel", "abc-formel", "quadratische gleichung", "midnight formula"], "x = (-b ± sqrt(b^2 - 4ac)) / (2a)", "x equals minus b, plus or minus the square root of b squared minus four a c, all over two a", "for a x^2 + b x + c = 0"),
    f("maths", "pq formula", &["pq formula", "pq-formel", "pq formel", "p q formula"], "x = -p/2 ± sqrt((p/2)^2 - q)", "x equals minus p over two, plus or minus the square root of p over two squared minus q", "for x^2 + p x + q = 0"),
    f("maths", "Pythagorean theorem", &["pythagoras", "pythagorean theorem", "satz des pythagoras", "hypotenuse"], "a^2 + b^2 = c^2", "a squared plus b squared equals c squared", "a, b: the legs of a right triangle; c: the hypotenuse"),
    f("maths", "Binomial formulas", &["binomial formula", "binomial formulas", "binomische formel", "binomische formeln"], "(a + b)^2 = a^2 + 2ab + b^2;  (a - b)^2 = a^2 - 2ab + b^2;  (a + b)(a - b) = a^2 - b^2", "a plus b squared is a squared plus two a b plus b squared; a minus b squared is a squared minus two a b plus b squared; a plus b times a minus b is a squared minus b squared", "the three binomial identities"),
    f("maths", "Power rule", &["power rule", "potenzregel", "derivative of a power", "ableitung potenz"], "f(x) = x^n  =>  f'(x) = n * x^(n-1)", "the derivative of x to the n is n times x to the n minus one", "n: any real exponent"),
    f("maths", "Product rule", &["product rule", "produktregel"], "(u * v)' = u' * v + u * v'", "the derivative of u times v is u prime v plus u v prime", "u, v: functions of x"),
    f("maths", "Quotient rule", &["quotient rule", "quotientenregel"], "(u / v)' = (u' * v - u * v') / v^2", "the derivative of u over v is u prime v minus u v prime, over v squared", "u, v: functions of x, v not zero"),
    f("maths", "Chain rule", &["chain rule", "kettenregel"], "(f(g(x)))' = f'(g(x)) * g'(x)", "the derivative of f of g is f prime of g, times g prime", "outer derivative times inner derivative"),
    f("maths", "Integral of a power", &["integral of a power", "power rule for integrals", "stammfunktion potenz", "integral potenz", "antiderivative"], "∫ x^n dx = x^(n+1) / (n+1) + C   (n ≠ -1)", "the integral of x to the n is x to the n plus one over n plus one, plus a constant", "C: the constant of integration"),
    f("maths", "Circle", &["circle area", "area of a circle", "circumference", "kreisfläche", "kreisumfang", "kreis"], "A = π r^2;  U = 2 π r", "the area is pi r squared, the circumference two pi r", "r: radius"),
    f("maths", "Sphere", &["sphere volume", "volume of a sphere", "kugelvolumen", "kugel", "sphere surface"], "V = 4/3 π r^3;  O = 4 π r^2", "the volume is four thirds pi r cubed, the surface four pi r squared", "r: radius"),
    f("maths", "Law of sines", &["law of sines", "sine rule", "sinussatz"], "a / sin(α) = b / sin(β) = c / sin(γ)", "a over sine alpha equals b over sine beta equals c over sine gamma", "each side over the sine of its opposite angle"),
    f("maths", "Law of cosines", &["law of cosines", "cosine rule", "kosinussatz", "cosinussatz"], "c^2 = a^2 + b^2 - 2ab cos(γ)", "c squared equals a squared plus b squared minus two a b cosine gamma", "γ: the angle opposite side c"),
    f("maths", "Compound interest", &["compound interest", "zinseszins", "interest formula"], "K_n = K_0 * (1 + p/100)^n", "the capital after n years is the start capital times one plus p over a hundred, to the n", "K_0: start capital; p: rate in percent; n: years"),
    f("maths", "Binomial distribution", &["binomial distribution", "binomialverteilung", "bernoulli formula", "bernoulli-formel"], "P(X = k) = (n over k) * p^k * (1 - p)^(n-k)", "the probability of k hits is n choose k, times p to the k, times one minus p to the n minus k", "n: trials; k: hits; p: probability of a hit"),
    f("maths", "Expected value and standard deviation", &["expected value", "erwartungswert", "standard deviation", "standardabweichung"], "μ = n * p;  σ = sqrt(n * p * (1 - p))", "mu is n times p; sigma is the square root of n p times one minus p", "for a binomial distribution"),
    // ---- Physics ----
    f("physics", "Kinetic energy", &["kinetic energy", "kinetische energie", "bewegungsenergie"], "E_kin = 1/2 * m * v^2", "kinetic energy is one half m v squared", "m: mass in kg; v: speed in m/s; E in joules"),
    f("physics", "Potential energy", &["potential energy", "potentielle energie", "lageenergie", "höhenenergie"], "E_pot = m * g * h", "potential energy is m g h", "m: mass; g: 9.81 m/s^2; h: height in m"),
    f("physics", "Newton's second law", &["newton's second law", "newtons second law", "force", "kraft", "zweites newtonsches gesetz", "f equals m a"], "F = m * a", "force is mass times acceleration", "F in newtons; m in kg; a in m/s^2"),
    f("physics", "Uniform acceleration", &["uniform acceleration", "equations of motion", "gleichmäßig beschleunigte bewegung", "beschleunigte bewegung", "distance under acceleration", "weg-zeit-gesetz"], "s = 1/2 * a * t^2 + v_0 * t;  v = a * t + v_0", "distance is one half a t squared plus v nought t; speed is a t plus v nought", "a: acceleration; t: time; v_0: starting speed"),
    f("physics", "Ohm's law", &["ohm's law", "ohms law", "ohmsches gesetz", "ohmsche gesetz", "voltage current resistance"], "U = R * I", "voltage is resistance times current", "U in volts; R in ohms; I in amperes"),
    f("physics", "Electrical power", &["electrical power", "elektrische leistung", "power formula", "leistung"], "P = U * I = R * I^2 = U^2 / R", "power is voltage times current", "P in watts"),
    f("physics", "Work and power", &["work", "arbeit", "mechanical work", "mechanische arbeit"], "W = F * s;  P = W / t", "work is force times distance; power is work over time", "W in joules; P in watts"),
    f("physics", "Momentum", &["momentum", "impuls"], "p = m * v", "momentum is mass times velocity", "p in kg m/s"),
    f("physics", "Wave equation", &["wave equation", "wave speed", "wellengleichung", "wellenlänge frequenz"], "c = λ * f", "wave speed is wavelength times frequency", "λ: wavelength in m; f: frequency in Hz"),
    f("physics", "Photon energy", &["photon energy", "photonenenergie", "quantum of action", "wirkungsquantum", "energie eines photons"], "E = h * f = h * c / λ", "the energy of a photon is the quantum of action h times its frequency", "h = 6.626e-34 J s"),
    f("physics", "Coulomb's law", &["coulomb's law", "coulombs law", "coulomb-gesetz", "coulombsches gesetz", "coulomb"], "F = 1/(4 π ε_0) * q_1 * q_2 / r^2", "the force is one over four pi epsilon nought, times q one q two over r squared", "ε_0 = 8.854e-12 As/(Vm); r: distance of the charges"),
    f("physics", "Capacitor", &["capacitor", "kondensator", "capacitance", "kapazität"], "C = Q / U;  W = 1/2 * C * U^2", "capacitance is charge over voltage; the stored energy is one half C U squared", "C in farads"),
    f("physics", "Lorentz force", &["lorentz force", "lorentzkraft"], "F = q * v * B", "the Lorentz force is q v B", "for a charge moving at right angles to the field B"),
    f("physics", "Induction", &["induction", "induktion", "faraday", "induktionsgesetz", "induced voltage"], "U_ind = -N * ΔΦ / Δt", "the induced voltage is minus N times the change of flux over time", "N: turns; Φ = B * A: magnetic flux"),
    f("physics", "Mass-energy equivalence", &["mass energy", "e equals m c squared", "einstein", "masse energie"], "E = m * c^2", "E equals m c squared", "c = 299,792,458 m/s"),
    f("physics", "Radioactive decay", &["radioactive decay", "half-life", "half life", "zerfallsgesetz", "halbwertszeit", "radioaktiver zerfall"], "N(t) = N_0 * e^(-λ t);  T_1/2 = ln(2) / λ", "the remaining nuclei are N nought times e to the minus lambda t; the half-life is the natural log of two over lambda", "λ: decay constant"),
    f("physics", "Pendulum", &["pendulum", "fadenpendel", "pendulum period", "schwingungsdauer"], "T = 2 π * sqrt(l / g)", "the period is two pi times the square root of l over g", "l: length of the pendulum; g: 9.81 m/s^2"),
    f("physics", "Density", &["density", "dichte"], "ρ = m / V", "density is mass over volume", "ρ in kg/m^3"),
    f("physics", "Pressure", &["pressure", "druck"], "p = F / A", "pressure is force over area", "p in pascals"),
    // ---- Chemistry ----
    f("chemistry", "Amount of substance", &["amount of substance", "moles", "stoffmenge", "mole formula", "n equals m over m"], "n = m / M", "the amount of substance is mass over molar mass", "n in mol; m in g; M in g/mol"),
    f("chemistry", "Concentration", &["concentration", "konzentration", "molarity", "stoffmengenkonzentration"], "c = n / V", "concentration is amount over volume", "c in mol/L; V in litres"),
    f("chemistry", "Ideal gas law", &["ideal gas law", "ideal gas", "ideales gasgesetz", "gasgleichung", "allgemeine gasgleichung"], "p * V = n * R * T", "p V equals n R T", "R = 8.314 J/(mol K); T in kelvin"),
    f("chemistry", "pH value", &["ph value", "ph", "ph-wert", "ph wert"], "pH = -log10(c(H3O+))", "pH is the negative decimal logarithm of the oxonium concentration", "c in mol/L"),
    f("chemistry", "Molar volume", &["molar volume", "molares volumen"], "V_m = V / n = 22.4 L/mol (0 °C, 1013 hPa)", "the molar volume of an ideal gas is twenty-two point four litres per mole at standard conditions", "24.5 L/mol at 25 °C"),
    f("chemistry", "Mass action law", &["mass action", "massenwirkungsgesetz", "equilibrium constant", "gleichgewichtskonstante"], "K_c = (c(C)^c * c(D)^d) / (c(A)^a * c(B)^b)", "the equilibrium constant is the product concentrations over the reactant concentrations, each to its coefficient", "for a A + b B ⇌ c C + d D"),
    f("chemistry", "Nernst equation", &["nernst", "nernst equation", "nernst-gleichung", "nernstgleichung"], "E = E° + (0.059 V / z) * log10(c(Ox) / c(Red))", "the potential is the standard potential plus zero point zero five nine volts over z, times the log of oxidised over reduced", "z: electrons transferred; at 25 °C"),
];

/// Small words of a question that say nothing about which formula.
const FILLER: &[&str] = &["the", "a", "an", "of", "for", "formula", "formulas", "equation", "law", "rule", "what", "is", "whats", "show", "me", "tell", "give", "die", "der", "das", "den", "formel", "für", "fuer", "zeig", "mir", "wie", "lautet", "nenn", "gib", "bitte", "please", "to", "calculate", "how", "do", "i", "you", "again", "nochmal", "gleichung", "gesetz", "regel", "von", "zur", "zum", "berechnung", "berechnen", "sir"];

fn words(text: &str) -> Vec<String> {
    text.to_lowercase().split(|c: char| !c.is_alphanumeric() && c != '\'').filter(|w| !w.is_empty()).map(String::from).collect()
}

/// The formulas a question means, best first (a key that is the whole question wins; else the
/// entries sharing the most words with it). `subject` narrows it down.
pub fn lookup(query: &str, subject: Option<&str>) -> Vec<&'static Formula> {
    let lower = query.to_lowercase();
    let asked: Vec<String> = words(&lower).into_iter().filter(|w| !FILLER.contains(&w.as_str())).collect();
    let phrase = asked.join(" ");
    if phrase.is_empty() {
        return Vec::new();
    }
    let mut scored: Vec<(u32, usize, &Formula)> = Vec::new();
    for (order, entry) in SHEET.iter().enumerate() {
        if subject.is_some_and(|s| !entry.subject.starts_with(&s.to_lowercase()[..s.len().min(4)])) {
            continue;
        }
        let mut best = 0u32;
        for key in entry.keys.iter().copied().chain([entry.name]) {
            let key_words: Vec<String> = words(key).into_iter().filter(|w| !FILLER.contains(&w.as_str())).collect();
            let key_phrase = key_words.join(" ");
            // One inside the other as whole words ("energy" in "kinetic energy", never "ph" in
            // "photon").
            let inside = |hay: &[String], needle: &[String]| !needle.is_empty() && hay.windows(needle.len()).any(|w| w == needle);
            let points = if key_phrase == phrase {
                100
            } else if inside(&asked, &key_words) || inside(&key_words, &asked) {
                60 + key_words.len() as u32
            } else {
                let shared = key_words.iter().filter(|w| asked.contains(w)).count();
                // Every word of the key asked for, or every word of the question in the key:
                // one shared word among others ("a good life" and "half-life") is no match.
                if shared > 0 && (shared == key_words.len() || shared == asked.len()) { 10 * shared as u32 } else { 0 }
            };
            best = best.max(points);
        }
        if best > 0 {
            scored.push((best, order, entry));
        }
    }
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    let top = scored.first().map_or(0, |s| s.0);
    // An exact name answers alone; otherwise everything about as good as the best.
    scored.into_iter().filter(|(points, _, _)| *points == top || (top < 100 && *points + 10 >= top)).take(4).map(|(_, _, e)| e).collect()
}

pub fn tool_definition() -> Value {
    json!({"type": "function", "function": {
        "name": "lookup_formula",
        "description": "The formula sheet: school maths, physics and chemistry formulas by name in English or German ('kinetic energy', 'pq-Formel', 'Ohm's law', 'ideal gas law'), written out with every symbol explained and shown as a block. Use it whenever Sir asks for a formula; never recite one from memory.",
        "parameters": {"type": "object", "properties": {
            "query": {"type": "string", "description": "What the formula is called or is for."},
            "subject": {"type": "string", "enum": ["maths", "physics", "chemistry"]}
        }, "required": ["query"]}
    }})
}

/// The `lookup_formula` tool: the formulas as a block, the best one read aloud.
pub fn run_tool(args: &Value) -> Result<String, String> {
    let query = crate::features::str_arg(args, &["query", "name", "formula", "topic"]).ok_or("Which formula, Sir?")?;
    let found = lookup(query, crate::features::str_arg(args, &["subject"]));
    let Some(first) = found.first() else {
        return Ok(crate::features::spoken(format!("No formula for '{query}' on the sheet."), format!("I have no formula for {query} on the sheet, Sir.")));
    };
    let block: Vec<String> = found.iter().map(|e| format!("{} ({})\n  {}\n  {}", e.name, e.subject, e.written, e.symbols)).collect();
    let more = if found.len() > 1 { format!(" The other {} are on the board.", found.len() - 1) } else { String::new() };
    Ok(crate::features::spoken(block.join("\n\n"), format!("{}: {}, Sir.{more}", first.name, first.spoken)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn first(query: &str) -> &'static str {
        lookup(query, None).first().map_or("(none)", |e| e.name)
    }

    #[test]
    fn a_formula_is_found_by_its_english_or_german_name() {
        for (asked, name) in [
            ("show me the formula for kinetic energy", "Kinetic energy"),
            ("kinetische Energie", "Kinetic energy"),
            ("what is the formula for the pq formula", "pq formula"),
            ("pq-Formel", "pq formula"),
            ("Mitternachtsformel", "Quadratic formula"),
            ("Ohm's law", "Ohm's law"),
            ("Ohmsches Gesetz", "Ohm's law"),
            ("wie lautet die Formel für die Kettenregel", "Chain rule"),
            ("ideal gas law", "Ideal gas law"),
            ("Satz des Pythagoras", "Pythagorean theorem"),
            ("the half-life", "Radioactive decay"),
            ("Massenwirkungsgesetz", "Mass action law"),
            ("pH-Wert", "pH value"),
            ("binomial distribution", "Binomial distribution"),
            ("Zinseszins", "Compound interest"),
        ] {
            assert_eq!(first(asked), name, "{asked}");
        }
        assert!(lookup("the weather in Berlin", None).is_empty());
        assert!(lookup("formula", None).is_empty() && lookup("", None).is_empty());
        // The subject narrows it down.
        assert!(lookup("energy", Some("physics")).iter().all(|e| e.subject == "physics"));
        assert!(lookup("energy", Some("chemistry")).is_empty());
    }

    #[test]
    fn every_entry_is_complete_plain_and_findable_by_each_of_its_keys() {
        for entry in SHEET {
            assert!(!entry.written.is_empty() && !entry.spoken.is_empty() && !entry.symbols.is_empty() && !entry.keys.is_empty(), "{}", entry.name);
            assert!(!entry.spoken.contains(['^', '*', '=', '_']), "{} is read aloud with symbols in it: {}", entry.name, entry.spoken);
            assert!(entry.written.contains('=') || entry.written.contains('⇌'), "{} is no equation: {}", entry.name, entry.written);
            for key in entry.keys {
                assert_eq!(*key, key.to_lowercase(), "{key}");
                assert!(lookup(key, None).iter().any(|found| found.name == entry.name), "'{key}' does not find {}", entry.name);
            }
            assert!(!entry.written.chars().chain(entry.symbols.chars()).any(crate::live_self_tester::is_pictograph), "{}", entry.name);
        }
        assert!(SHEET.len() >= 40);
    }

    #[test]
    fn the_tool_shows_the_formula_and_reads_it_without_symbols() {
        let said = run_tool(&json!({"query": "kinetic energy"})).unwrap();
        assert!(said.contains("E_kin = 1/2 * m * v^2") && said.contains("m: mass in kg"), "{said}");
        assert!(said.ends_with("Spoken: Kinetic energy: kinetic energy is one half m v squared, Sir."), "{said}");
        let said = run_tool(&json!({"query": "unicorn drive"})).unwrap();
        assert!(said.contains("No formula for 'unicorn drive'"), "{said}");
        assert!(run_tool(&json!({})).is_err());
    }
}

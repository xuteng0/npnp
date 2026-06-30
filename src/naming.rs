use crate::schlib::SchlibParameter;

fn designator_stem(designator: &str) -> String {
    designator
        .trim()
        .trim_end_matches(|c: char| c == '?' || c.is_ascii_digit())
        .trim()
        .to_ascii_uppercase()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PassiveKind {
    Res,
    Cap,
    Ind,
    Fb,
}

impl PassiveKind {
    fn prefix(self) -> &'static str {
        match self {
            Self::Res => "RES",
            Self::Cap => "CAP",
            Self::Ind => "IND",
            Self::Fb => "FB",
        }
    }
}

fn classify_passive_kind(designator: &str, parameters: &[SchlibParameter]) -> Option<PassiveKind> {
    let stem = designator_stem(designator);
    let category = find_param(parameters, &["category"])
        .map(|value| value.to_ascii_lowercase())
        .unwrap_or_default();
    let is_ferrite_bead = category.contains("ferrite") && category.contains("bead");

    match stem.as_str() {
        "FB" => Some(PassiveKind::Fb),
        "L" => {
            if is_ferrite_bead {
                Some(PassiveKind::Fb)
            } else {
                Some(PassiveKind::Ind)
            }
        }
        "C" | "CV" => Some(PassiveKind::Cap),
        "R" | "RV" | "VR" => Some(PassiveKind::Res),
        _ if is_ferrite_bead => Some(PassiveKind::Fb),
        _ => None,
    }
}

fn normalize_lcsc_token(lcsc_id: Option<&str>) -> Option<String> {
    let raw = lcsc_id?.trim();
    let digits = raw.strip_prefix('C').or_else(|| raw.strip_prefix('c'))?;
    if digits.is_empty() || !digits.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    Some(format!("C{digits}"))
}

fn normalize_freeform_token(value: &str) -> Option<String> {
    let mut token = String::new();
    let mut last_was_separator = false;

    for ch in value.trim().chars() {
        let mapped = match ch {
            'a'..='z' | 'A'..='Z' => Some(ch.to_ascii_uppercase()),
            '0'..='9' => Some(ch),
            ' ' | '/' | '\\' | ',' | '(' | ')' | '[' | ']' | '{' | '}' | '-' | '_' => Some('_'),
            '.' => Some('D'),
            '\u{00B5}' | '\u{03BC}' => Some('U'),
            '\u{03A9}' | '\u{2126}' => Some('R'),
            '%' => Some('P'),
            '\u{00D7}' => Some('X'),
            _ => None,
        };

        match mapped {
            Some('_') => {
                if !token.is_empty() && !last_was_separator {
                    token.push('_');
                    last_was_separator = true;
                }
            }
            Some(c) => {
                token.push(c);
                last_was_separator = false;
            }
            None => {}
        }
    }

    let token = token.trim_matches('_').to_string();
    (!token.is_empty()).then_some(token)
}

fn find_param<'a>(parameters: &'a [SchlibParameter], keywords: &[&str]) -> Option<&'a str> {
    parameters
        .iter()
        .find(|p| {
            let name = p.name.to_ascii_lowercase();
            keywords.iter().any(|keyword| name.contains(keyword))
        })
        .map(|p| p.value.as_str())
}

fn normalize_decimal_dimension(value: &str) -> Option<String> {
    let mut token = String::new();
    for ch in value.trim().chars() {
        match ch {
            '0'..='9' => token.push(ch),
            '.' => token.push('D'),
            _ => {}
        }
    }
    (!token.is_empty()).then_some(token)
}

fn normalize_package(value: &str) -> Option<String> {
    let compact = value.trim().replace(' ', "").replace('\u{00D7}', "X");
    if compact.is_empty() {
        return None;
    }
    let upper = compact.to_ascii_uppercase();

    if upper.chars().all(|c| c.is_ascii_digit()) {
        return Some(upper);
    }

    if let Some(rest) = upper.strip_prefix("SMD,").or_else(|| upper.strip_prefix("SMD")) {
        let dims = rest.trim_start_matches(',');
        let dims = dims.strip_suffix("MM").unwrap_or(dims);
        if let Some((left, right)) = dims.split_once('X') {
            let left = normalize_decimal_dimension(left)?;
            let right = normalize_decimal_dimension(right)?;
            return Some(format!("SMD{left}X{right}"));
        }
    }

    normalize_freeform_token(&upper)
}

fn parse_number_and_suffix(value: &str) -> Option<(f64, String)> {
    let compact = value
        .trim()
        .replace(' ', "")
        .replace(',', "")
        .replace('\u{2126}', "\u{03A9}");
    let compact = compact.trim_start_matches('\u{00B1}').trim_start_matches('+');
    let mut seen_digit = false;
    let mut seen_dot = false;
    let mut end = 0usize;

    for (idx, ch) in compact.char_indices() {
        if ch.is_ascii_digit() {
            seen_digit = true;
            end = idx + ch.len_utf8();
        } else if ch == '.' && !seen_dot {
            seen_dot = true;
            end = idx + ch.len_utf8();
        } else {
            break;
        }
    }

    if !seen_digit {
        return None;
    }

    let number = compact[..end].parse::<f64>().ok()?;
    Some((number, compact[end..].to_string()))
}

fn format_marker_token(value: f64, marker: &str) -> Option<String> {
    if !value.is_finite() || value < 0.0 {
        return None;
    }

    let mut text = format!("{value:.9}");
    while text.contains('.') && text.ends_with('0') {
        text.pop();
    }
    if text.ends_with('.') {
        text.pop();
    }

    let token = if let Some((whole, frac)) = text.split_once('.') {
        if frac.is_empty() {
            format!("{whole}{marker}")
        } else {
            format!("{whole}{marker}{frac}")
        }
    } else {
        format!("{text}{marker}")
    };

    Some(token)
}

fn normalize_resistance(value: &str) -> Option<String> {
    let compact = value.trim().replace(' ', "");
    let prebuilt = compact.to_ascii_uppercase();
    if prebuilt.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        && (prebuilt.contains('R') || prebuilt.contains('K') || prebuilt.contains('M'))
        && !prebuilt.contains("OHM")
    {
        return Some(prebuilt);
    }

    let (number, suffix) = parse_number_and_suffix(value)?;
    let scale = if suffix.starts_with('m')
        && (suffix[1..].to_ascii_lowercase().contains("ohm")
            || suffix.contains('\u{03A9}')
            || suffix.contains('\u{2126}'))
    {
        1e-3
    } else if suffix.starts_with('K') || suffix.starts_with('k') {
        1e3
    } else if suffix.starts_with('M') {
        1e6
    } else if suffix.is_empty()
        || suffix.starts_with('R')
        || suffix.eq_ignore_ascii_case("OHM")
        || suffix.contains('\u{03A9}')
    {
        1.0
    } else {
        return normalize_freeform_token(value);
    };

    let ohms = number * scale;
    if ohms >= 1_000_000.0 {
        format_marker_token(ohms / 1_000_000.0, "M")
    } else if ohms >= 1_000.0 {
        format_marker_token(ohms / 1_000.0, "K")
    } else {
        format_marker_token(ohms, "R")
    }
}

fn normalize_capacitance(value: &str) -> Option<String> {
    let compact = value
        .trim()
        .replace(' ', "")
        .replace('\u{00B5}', "u")
        .replace('\u{03BC}', "u");
    let prebuilt = compact.to_ascii_uppercase();
    if prebuilt.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        && (prebuilt.contains('P') || prebuilt.contains('N') || prebuilt.contains('U'))
        && !prebuilt.contains('F')
    {
        return Some(prebuilt);
    }

    let (number, suffix) = parse_number_and_suffix(&compact)?;
    let scale = if suffix.starts_with('p') || suffix.starts_with('P') {
        1e-12
    } else if suffix.starts_with('n') || suffix.starts_with('N') {
        1e-9
    } else if suffix.starts_with('u') || suffix.starts_with('U') {
        1e-6
    } else if suffix.starts_with('m') {
        1e-3
    } else {
        return normalize_freeform_token(value);
    };

    let farads = number * scale;
    if farads >= 1e-6 {
        format_marker_token(farads * 1e6, "U")
    } else if farads >= 1e-9 {
        format_marker_token(farads * 1e9, "N")
    } else {
        format_marker_token(farads * 1e12, "P")
    }
}

fn normalize_inductance(value: &str) -> Option<String> {
    let compact = value
        .trim()
        .replace(' ', "")
        .replace('\u{00B5}', "u")
        .replace('\u{03BC}', "u");
    let prebuilt = compact.to_ascii_uppercase();
    if prebuilt.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        && (prebuilt.contains('N') || prebuilt.contains('U'))
        && !prebuilt.contains('H')
    {
        return Some(prebuilt);
    }

    let (number, suffix) = parse_number_and_suffix(&compact)?;
    let scale = if suffix.starts_with('n') || suffix.starts_with('N') {
        1e-9
    } else if suffix.starts_with('u') || suffix.starts_with('U') {
        1e-6
    } else if suffix.starts_with('m') || suffix.starts_with('M') {
        1e-3
    } else {
        return normalize_freeform_token(value);
    };

    let henries = number * scale;
    if henries >= 1e-6 {
        format_marker_token(henries * 1e6, "U")
    } else {
        format_marker_token(henries * 1e9, "N")
    }
}

fn normalize_voltage(value: &str) -> Option<String> {
    let (number, suffix) = parse_number_and_suffix(value)?;
    if suffix.is_empty() || suffix.eq_ignore_ascii_case("V") {
        format_marker_token(number, "V")
    } else {
        normalize_freeform_token(value)
    }
}

fn normalize_tolerance(value: &str) -> Option<String> {
    let (number, suffix) = parse_number_and_suffix(value)?;
    if suffix.contains('%') || suffix.is_empty() {
        format_marker_token(number, "P")
    } else {
        normalize_freeform_token(value)
    }
}

fn normalize_power(value: &str) -> Option<String> {
    let (number, suffix) = parse_number_and_suffix(value)?;
    if suffix.starts_with('m') || suffix.starts_with('M') {
        let mw = number;
        let token = format_marker_token(mw, "M")?;
        Some(format!("{token}W"))
    } else if suffix.is_empty() || suffix.eq_ignore_ascii_case("W") {
        format_marker_token(number, "W")
    } else {
        normalize_freeform_token(value)
    }
}

fn normalize_current(value: &str) -> Option<String> {
    let (number, suffix) = parse_number_and_suffix(value)?;
    if suffix.starts_with('m') {
        let token = format_marker_token(number, "M")?;
        Some(format!("{token}A"))
    } else if suffix.is_empty() || suffix.eq_ignore_ascii_case("A") {
        format_marker_token(number, "A")
    } else {
        normalize_freeform_token(value)
    }
}

fn normalize_dcr(value: &str) -> Option<String> {
    let token = normalize_resistance(value)?;
    if token.contains('K') || token.contains('M') {
        return None;
    }

    let (number, suffix) = parse_number_and_suffix(value)?;
    let scale = if suffix.starts_with('m')
        && (suffix[1..].to_ascii_lowercase().contains("ohm")
            || suffix.contains('\u{03A9}')
            || suffix.contains('\u{2126}'))
    {
        1e-3
    } else if suffix.is_empty()
        || suffix.starts_with('R')
        || suffix.eq_ignore_ascii_case("OHM")
        || suffix.contains('\u{03A9}')
    {
        1.0
    } else {
        return None;
    };
    let milli_ohms = number * scale * 1000.0;
    let token = format_marker_token(milli_ohms, "M")?;
    Some(format!("{token}OR"))
}

fn normalize_frequency(value: &str) -> Option<String> {
    let (number, suffix) = parse_number_and_suffix(value)?;
    if suffix.starts_with('G') || suffix.starts_with('g') {
        Some(format!("{}HZ", format_marker_token(number, "G")?))
    } else if suffix.starts_with('M') {
        Some(format!("{}HZ", format_marker_token(number, "M")?))
    } else if suffix.starts_with('K') || suffix.starts_with('k') {
        Some(format!("{}HZ", format_marker_token(number, "K")?))
    } else if suffix.eq_ignore_ascii_case("HZ") || suffix.is_empty() {
        Some(format!("{}HZ", format_marker_token(number, "")?))
    } else {
        normalize_freeform_token(value)
    }
}

fn normalize_dielectric(value: &str) -> Option<String> {
    normalize_freeform_token(value)
}

fn normalize_inductor_type(parameters: &[SchlibParameter]) -> Option<String> {
    if let Some(raw) = find_param(parameters, &["type", "construction"]) {
        let lower = raw.to_ascii_lowercase();
        if lower.contains("unshielded") {
            return Some("UNSHIELDED".to_string());
        }
        if lower.contains("shielded") {
            return Some("SHIELDED".to_string());
        }
        if lower.contains("molded") {
            return Some("MOLDED".to_string());
        }
        if let Some(token) = normalize_freeform_token(raw) {
            return Some(token);
        }
    }

    for param in parameters {
        let lower = format!("{} {}", param.name, param.value).to_ascii_lowercase();
        if lower.contains("unshielded") {
            return Some("UNSHIELDED".to_string());
        }
        if lower.contains("shielded") {
            return Some("SHIELDED".to_string());
        }
        if lower.contains("molded") {
            return Some("MOLDED".to_string());
        }
    }

    None
}

fn normalize_fb_impedance(parameters: &[SchlibParameter]) -> Option<String> {
    let raw = find_param(parameters, &["impedance"])?;
    let base = raw.split_once('@').map(|(left, _)| left).unwrap_or(raw);
    normalize_resistance(base)
}

fn normalize_fb_frequency(parameters: &[SchlibParameter]) -> Option<String> {
    if let Some(raw) = find_param(parameters, &["frequency"]) {
        return normalize_frequency(raw);
    }
    let raw = find_param(parameters, &["impedance"])?;
    let (_, freq) = raw.split_once('@')?;
    normalize_frequency(freq)
}

fn build_name_from_tokens(
    kind: PassiveKind,
    tokens: Vec<Option<String>>,
    lcsc_id: Option<&str>,
) -> Option<String> {
    let lcsc = normalize_lcsc_token(lcsc_id)?;
    let mut parts = vec![kind.prefix().to_string()];
    parts.extend(
        tokens
            .into_iter()
            .flatten()
            .filter(|token| !token.trim().is_empty()),
    );
    if parts.len() <= 1 {
        return None;
    }
    parts.push(lcsc);
    Some(parts.join("_"))
}

fn detect_ecap_prefix(category: &str) -> &'static str {
    let cat = category.to_ascii_uppercase();
    if cat.contains("POLYMER") {
        "PCAP"
    } else if cat.contains("TANTALUM") {
        "TANT"
    } else if cat.contains("FILM") {
        "FCAP"
    } else {
        "ECAP"
    }
}

fn detect_ecap_mounting(category: &str, params: &[SchlibParameter]) -> Option<&'static str> {
    if let Some(mounting) = find_param(params, &["mounting"]) {
        let lower = mounting.to_ascii_lowercase();
        if lower.contains("smd") || lower.contains("surface") {
            return Some("SMD");
        }
        if lower.contains("through")
            || lower.contains("radial")
            || lower.contains("leaded")
            || lower.contains("plug")
        {
            return Some("RAD");
        }
        if lower.contains("axial") {
            return Some("AXIAL");
        }
        if lower.contains("snap") {
            return Some("SNAPIN");
        }
    }
    let cat = category.to_ascii_uppercase();
    if cat.contains("SMD") || cat.contains("SURFACE") {
        Some("SMD")
    } else if cat.contains("LEADED") || cat.contains("THROUGH") || cat.contains("RADIAL") {
        Some("RAD")
    } else {
        None
    }
}

fn extract_dim_numbers(s: &str) -> Vec<f64> {
    let mut result = Vec::new();
    let mut current = String::new();
    for ch in s.chars() {
        if ch.is_ascii_digit() || (ch == '.' && !current.contains('.') && !current.is_empty()) {
            current.push(ch);
        } else if !current.is_empty() {
            if let Ok(n) = current.parse::<f64>() {
                if n > 0.0 {
                    result.push(n);
                }
            }
            current.clear();
        }
    }
    if !current.is_empty() {
        if let Ok(n) = current.parse::<f64>() {
            if n > 0.0 {
                result.push(n);
            }
        }
    }
    result
}

fn format_dim(value: f64) -> String {
    format!("{:.1}", value)
}

fn parse_ecap_smd_size(params: &[SchlibParameter]) -> Option<String> {
    let pkg = find_param(params, &["package", "case", "size"])?;
    let nums = extract_dim_numbers(pkg);
    if nums.len() >= 2 {
        Some(format!("D{}H{}", format_dim(nums[0]), format_dim(nums[1])))
    } else {
        None
    }
}

fn standard_rad_cap_pitch(diam_mm: f64) -> Option<f64> {
    // IEC 60384 / industry-standard lead pitch by body diameter
    if diam_mm < 5.5 {
        Some(2.0)
    } else if diam_mm < 7.5 {
        Some(2.5)
    } else if diam_mm < 9.5 {
        Some(3.5)
    } else if diam_mm < 14.0 {
        Some(5.0)
    } else if diam_mm < 19.0 {
        Some(7.5)
    } else {
        None
    }
}

fn parse_ecap_rad_size(params: &[SchlibParameter]) -> Option<String> {
    let pkg = find_param(params, &["package", "case", "size"])?;
    let nums = extract_dim_numbers(pkg);
    let diam = nums.first().copied()?;
    let pitch = find_param(params, &["lead spacing", "pin spacing", "pitch"])
        .and_then(|v| extract_dim_numbers(v).into_iter().next())
        .or_else(|| standard_rad_cap_pitch(diam));
    if let Some(p) = pitch {
        Some(format!("D{}_P{}", format_dim(diam), format_dim(p)))
    } else {
        Some(format!("D{}", format_dim(diam)))
    }
}

fn parse_tant_case(params: &[SchlibParameter]) -> Option<String> {
    let pkg = find_param(params, &["package", "case"])?;
    let upper = pkg.to_ascii_uppercase();
    if let Some(idx) = upper.find("CASE") {
        let rest =
            upper[idx + 4..].trim_start_matches(|c: char| c == '_' || c == ' ' || c == '-');
        if let Some(letter) = rest.chars().next() {
            if letter.is_ascii_uppercase() {
                return Some(format!("CASE{letter}"));
            }
        }
    }
    for (code, case) in [
        ("3216", "CASEA"),
        ("1206", "CASEA"),
        ("3528", "CASEB"),
        ("6032", "CASEC"),
        ("7343", "CASED"),
        ("7360", "CASEE"),
    ] {
        if upper.contains(code) {
            return Some(case.to_string());
        }
    }
    None
}

fn detect_ecap_optional_features(params: &[SchlibParameter]) -> Vec<String> {
    let mut features: Vec<String> = Vec::new();
    let mut has_125c = false;
    for param in params {
        let combined = format!("{} {}", param.name, param.value).to_ascii_uppercase();
        if (combined.contains("LOW") && combined.contains("ESR")) || combined.contains("LOWESR") {
            if !features.contains(&"LOWESR".to_string()) {
                features.push("LOWESR".to_string());
            }
        }
        if (combined.contains("HIGH") && combined.contains("RIPPLE"))
            || combined.contains("HIGHRIPPLE")
        {
            if !features.contains(&"HIGHRIPPLE".to_string()) {
                features.push("HIGHRIPPLE".to_string());
            }
        }
        if (combined.contains("LONG") && combined.contains("LIFE"))
            || combined.contains("LONGLIFE")
        {
            if !features.contains(&"LONGLIFE".to_string()) {
                features.push("LONGLIFE".to_string());
            }
        }
        if combined.contains("125") && combined.contains("C") {
            if !features.contains(&"125C".to_string()) {
                features.push("125C".to_string());
                has_125c = true;
            }
        } else if combined.contains("105") && combined.contains("C") && !has_125c {
            if !features.contains(&"105C".to_string()) {
                features.push("105C".to_string());
            }
        }
        if combined.contains("AEC") || combined.contains("AUTOMOTIVE") {
            if !features.contains(&"AECQ200".to_string()) {
                features.push("AECQ200".to_string());
            }
        }
        if combined.contains("AUDIO") {
            if !features.contains(&"AUDIO".to_string()) {
                features.push("AUDIO".to_string());
            }
        }
    }
    features
}

/// Build a component name for polarized capacitors following the ECAP convention:
/// `ECAP_<VALUE>_<VOLTAGE>_[FEATURES_]<MOUNT>_<SIZE>_<LCSC>`
pub(crate) fn build_ecap_component_name(
    category: &str,
    params: &[SchlibParameter],
    lcsc_id: Option<&str>,
) -> Option<String> {
    let lcsc = normalize_lcsc_token(lcsc_id)?;
    let prefix = detect_ecap_prefix(category);
    let capacitance = find_param(params, &["capacitance"]).and_then(normalize_capacitance)?;
    let voltage = find_param(params, &["voltage"]).and_then(normalize_voltage)?;
    let features = detect_ecap_optional_features(params);

    let name = if prefix == "TANT" {
        let case = parse_tant_case(params);
        let mut parts = vec![prefix.to_string(), capacitance, voltage];
        parts.extend(features);
        if let Some(c) = case {
            parts.push(c);
        }
        parts.push(lcsc);
        parts.join("_")
    } else {
        let mounting = detect_ecap_mounting(category, params).unwrap_or("RAD");
        let size = match mounting {
            "SMD" => parse_ecap_smd_size(params),
            _ => parse_ecap_rad_size(params),
        };
        let mut parts = vec![prefix.to_string(), capacitance, voltage];
        parts.extend(features);
        parts.push(mounting.to_string());
        if let Some(s) = size {
            parts.push(s);
        }
        parts.push(lcsc);
        parts.join("_")
    };

    name.chars()
        .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_' || c == '.')
        .then_some(name)
}

pub(crate) fn build_passive_component_name(
    designator: &str,
    parameters: &[SchlibParameter],
    lcsc_id: Option<&str>,
) -> Option<String> {
    let kind = classify_passive_kind(designator, parameters)?;

    let name = match kind {
        PassiveKind::Res => build_name_from_tokens(
            kind,
            vec![
                find_param(parameters, &["resistance"]).and_then(normalize_resistance),
                find_param(parameters, &["package", "case"]).and_then(normalize_package),
                find_param(parameters, &["tolerance"]).and_then(normalize_tolerance),
                find_param(parameters, &["power"]).and_then(normalize_power),
            ],
            lcsc_id,
        ),
        PassiveKind::Cap => build_name_from_tokens(
            kind,
            vec![
                find_param(parameters, &["capacitance"]).and_then(normalize_capacitance),
                find_param(parameters, &["package", "case"]).and_then(normalize_package),
                find_param(parameters, &["voltage"]).and_then(normalize_voltage),
                find_param(parameters, &["dielectric", "temperature characteristic"])
                    .and_then(normalize_dielectric),
                find_param(parameters, &["tolerance"]).and_then(normalize_tolerance),
            ],
            lcsc_id,
        ),
        PassiveKind::Ind => build_name_from_tokens(
            kind,
            vec![
                find_param(parameters, &["inductance"]).and_then(normalize_inductance),
                find_param(parameters, &["package", "case"]).and_then(normalize_package),
                find_param(parameters, &["current"]).and_then(normalize_current),
                find_param(parameters, &["dcr", "dc resistance"]).and_then(normalize_dcr),
                normalize_inductor_type(parameters),
            ],
            lcsc_id,
        ),
        PassiveKind::Fb => build_name_from_tokens(
            kind,
            vec![
                normalize_fb_impedance(parameters),
                normalize_fb_frequency(parameters),
                find_param(parameters, &["package", "case"]).and_then(normalize_package),
                find_param(parameters, &["current"]).and_then(normalize_current),
            ],
            lcsc_id,
        ),
    }?;

    name.chars()
        .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_' || c == '-')
        .then_some(name)
}
#[cfg(test)]
mod tests {
    use super::*;

    fn test_param(name: &str, value: &str) -> SchlibParameter {
        SchlibParameter {
            name: name.to_string(),
            value: value.to_string(),
        }
    }

    #[test]
    fn builds_resistor_name_per_lcsc_rule() {
        let params = vec![
            test_param("Resistance", "1m\u{03A9}"),
            test_param("Package", "2512"),
            test_param("Tolerance", "\u{00B1}1%"),
            test_param("Power", "3W"),
        ];
        assert_eq!(
            build_passive_component_name("R?", &params, Some("C2903470")).as_deref(),
            Some("RES_0R001_2512_1P_3W_C2903470")
        );
    }

    #[test]
    fn builds_capacitor_name_per_lcsc_rule() {
        let params = vec![
            test_param("Capacitance", "100nF"),
            test_param("Package", "0402"),
            test_param("Voltage", "16V"),
            test_param("Dielectric", "X7R"),
            test_param("Tolerance", "\u{00B1}10%"),
        ];
        assert_eq!(
            build_passive_component_name("C?", &params, Some("C1525")).as_deref(),
            Some("CAP_100N_0402_16V_X7R_10P_C1525")
        );
    }

    #[test]
    fn builds_inductor_name_per_lcsc_rule() {
        let params = vec![
            test_param("Inductance", "4.7uH"),
            test_param("Package", "SMD,6x6mm"),
            test_param("Current Rating", "3.3A"),
            test_param("DCR", "31m\u{03A9}"),
            test_param("Type", "Shielded"),
        ];
        assert_eq!(
            build_passive_component_name("L?", &params, Some("C5291878")).as_deref(),
            Some("IND_4U7_SMD6X6_3A3_31MOR_SHIELDED_C5291878")
        );
    }

    #[test]
    fn builds_ferrite_bead_name_per_lcsc_rule() {
        let params = vec![
            test_param("Category", "Ferrite Beads"),
            test_param("Impedance", "600\u{03A9}"),
            test_param("Frequency", "100MHz"),
            test_param("Package", "0603"),
            test_param("Current Rating", "300mA"),
        ];
        assert_eq!(
            build_passive_component_name("L?", &params, Some("C3716403")).as_deref(),
            Some("FB_600R_100MHZ_0603_300MA_C3716403")
        );
    }

    #[test]
    fn builds_ecap_radial_name_with_explicit_pitch() {
        let params = vec![
            test_param("Capacitance", "100\u{00B5}F"),
            test_param("Voltage", "50V"),
            test_param("Package / Case", "\u{03A6}8\u{00D7}10mm"),
            test_param("Lead Spacing", "3.5mm"),
        ];
        assert_eq!(
            build_ecap_component_name(
                "Aluminum Electrolytic Capacitors - Leaded",
                &params,
                Some("C970687")
            )
            .as_deref(),
            Some("ECAP_100U_50V_RAD_D8.0_P3.5_C970687")
        );
    }

    #[test]
    fn builds_ecap_radial_name_uses_standard_pitch_fallback() {
        // LCSC often returns "SMD,D8xL10.2mm" without a Lead Spacing property.
        // Standard pitch for 8mm body = 3.5mm.
        let params = vec![
            test_param("Capacitance", "100uF"),
            test_param("Voltage Rating", "50V"),
            test_param("Package", "SMD,D8xL10.2mm"),
        ];
        assert_eq!(
            build_ecap_component_name(
                "Capacitors/Aluminum Electrolytic Capacitors",
                &params,
                Some("C970687")
            )
            .as_deref(),
            Some("ECAP_100U_50V_RAD_D8.0_P3.5_C970687")
        );
    }

    #[test]
    fn builds_ecap_smd_name() {
        let params = vec![
            test_param("Capacitance", "100\u{00B5}F"),
            test_param("Voltage", "25V"),
            test_param("Package / Case", "\u{03A6}6.3\u{00D7}5.4mm"),
        ];
        assert_eq!(
            build_ecap_component_name(
                "Aluminum Electrolytic Capacitors - SMD",
                &params,
                Some("C12345")
            )
            .as_deref(),
            Some("ECAP_100U_25V_SMD_D6.3H5.4_C12345")
        );
    }

    #[test]
    fn builds_tant_name_from_case_code() {
        let params = vec![
            test_param("Capacitance", "47\u{00B5}F"),
            test_param("Voltage", "10V"),
            test_param("Package / Case", "3528-21"),
        ];
        assert_eq!(
            build_ecap_component_name("Tantalum Capacitors", &params, Some("C99999")).as_deref(),
            Some("TANT_47U_10V_CASEB_C99999")
        );
    }

    #[test]
    fn builds_ecap_name_with_decimal_voltage() {
        let params = vec![
            test_param("Capacitance", "100\u{00B5}F"),
            test_param("Voltage", "6.3V"),
            test_param("Package / Case", "\u{03A6}5\u{00D7}5.4mm"),
        ];
        assert_eq!(
            build_ecap_component_name(
                "Aluminum Electrolytic Capacitors - SMD",
                &params,
                Some("C11111")
            )
            .as_deref(),
            Some("ECAP_100U_6V3_SMD_D5.0H5.4_C11111")
        );
    }

    #[test]
    fn omits_missing_optional_fields_without_empty_underscores() {
        let params = vec![
            test_param("Resistance", "10k\u{03A9}"),
            test_param("Package", "0402"),
        ];
        assert_eq!(
            build_passive_component_name("R?", &params, Some("C25804")).as_deref(),
            Some("RES_10K_0402_C25804")
        );
    }
}

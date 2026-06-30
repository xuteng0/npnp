#[derive(Debug, Clone, Default)]
pub struct LcscProduct {
    pub sku: String,
    pub mpn: Option<String>,
    pub manufacturer: Option<String>,
    pub description: Option<String>,
    pub category: Option<String>,
    pub datasheet_url: Option<String>,
    pub properties: Vec<LcscProperty>,
}

impl LcscProduct {
    /// Returns the effective category: `self.category` if set, otherwise the value
    /// of a "Category" entry in `self.properties` (as returned by the LCSC English API).
    pub fn effective_category(&self) -> Option<&str> {
        if let Some(cat) = &self.category {
            return Some(cat.as_str());
        }
        self.properties
            .iter()
            .find(|p| p.name.eq_ignore_ascii_case("Category"))
            .map(|p| p.value.as_str())
    }

    /// Returns true when the LCSC category indicates a polarized capacitor
    /// (electrolytic, tantalum, polymer, supercapacitor). Non-polarized ceramics
    /// (MLCC) and film caps return false. Returns false when category is unknown.
    pub fn is_polarized_capacitor(&self) -> bool {
        let Some(cat) = self.effective_category() else { return false };
        let upper = cat.to_ascii_uppercase();
        upper.contains("ELECTROLYTIC")
            || upper.contains("TANTALUM")
            || upper.contains("POLYMER")
            || upper.contains("SUPERCAPACITOR")
            || upper.contains("SUPER CAPACITOR")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn product_with_category(cat: &str) -> LcscProduct {
        LcscProduct { category: Some(cat.to_string()), ..Default::default() }
    }

    #[test]
    fn mlcc_is_not_polarized() {
        assert!(!product_with_category("Multilayer Ceramic Capacitors MLCC - SMD/SMT").is_polarized_capacitor());
        assert!(!product_with_category("Multilayer Ceramic Capacitors MLCC - Leaded").is_polarized_capacitor());
    }

    #[test]
    fn electrolytic_is_polarized() {
        assert!(product_with_category("Aluminum Electrolytic Capacitors").is_polarized_capacitor());
        assert!(product_with_category("Aluminum Electrolytic Capacitors - SMD").is_polarized_capacitor());
    }

    #[test]
    fn tantalum_is_polarized() {
        assert!(product_with_category("Tantalum Capacitors").is_polarized_capacitor());
    }

    #[test]
    fn unknown_category_is_not_polarized() {
        let p = LcscProduct { category: None, ..Default::default() };
        assert!(!p.is_polarized_capacitor());
    }

    #[test]
    fn electrolytic_is_polarized_via_property() {
        let p = LcscProduct {
            category: None,
            properties: vec![LcscProperty {
                name: "Category".to_string(),
                value: "Capacitors/Aluminum Electrolytic Capacitors".to_string(),
            }],
            ..Default::default()
        };
        assert!(p.is_polarized_capacitor());
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LcscProperty {
    pub name: String,
    pub value: String,
}

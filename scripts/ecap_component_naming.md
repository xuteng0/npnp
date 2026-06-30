# Electrolytic Capacitor Component Naming Convention

This convention is for **Altium component names / Design Item IDs** for exact purchasable electrolytic capacitors, especially LCSC-generated components.

It is not for generic schematic symbol names or footprint names.

---

## 1. Purpose

A component name should identify the exact component clearly enough that it is readable in the schematic/library browser without opening the full parameter list.

For electrolytic capacitors, the name should capture:

- capacitor family / chemistry
- capacitance
- rated voltage
- mounting style
- package size
- important electrical or temperature options when useful
- LCSC part number for uniqueness

---

## 2. General Format

```text
<PREFIX>_<VALUE>_<VOLTAGE>_<MOUNT>_<SIZE>_<OPTIONAL_FEATURES>_<LCSC>
```

Recommended examples:

```text
ECAP_100U_25V_SMD_D6.3H5.4_C12345
ECAP_470U_35V_RAD_D10.0_P5.0_C12345
ECAP_220U_16V_LOWESR_RAD_D8.0_P3.5_C12345
PCAP_100U_16V_SMD_D6.3H5.8_C12345
TANT_47U_10V_CASEB_C12345
```

---

## 3. Prefix Rules

Use different prefixes for different capacitor technologies.

| Capacitor type | Prefix | Example |
|---|---:|---|
| Aluminum electrolytic capacitor | `ECAP` | `ECAP_100U_25V_SMD_D6.3H5.4_C12345` |
| Polymer electrolytic capacitor | `PCAP` | `PCAP_100U_16V_SMD_D6.3H5.8_C12345` |
| Tantalum capacitor | `TANT` | `TANT_47U_10V_CASEB_C12345` |
| Film capacitor | `FCAP` | `FCAP_1U_100V_P7.5_C12345` |
| Ceramic capacitor / MLCC | `CAP` | `CAP_100N_50V_0402_C12345` |

Do not use generic `CAP` for aluminum electrolytic capacitors because `CAP_100U_25V` could also refer to MLCC, tantalum, polymer, or film capacitors.

---

## 4. Capacitance Value Rules

Use ASCII-only unit suffixes.

| Actual value | Name field |
|---:|---:|
| 1 pF | `1P` |
| 100 nF | `100N` |
| 1 uF | `1U` |
| 10 uF | `10U` |
| 100 uF | `100U` |
| 1000 uF | `1000U` |

Rules:

- Use `P`, `N`, `U`; avoid `pF`, `nF`, `uF`, and `µF` in the component name.
- Use decimal notation only when needed.
- Replace decimal point with `R` if required by the broader naming convention.

Examples:

```text
ECAP_4U7_25V_SMD_D5.0H5.4_C12345
ECAP_22U_50V_RAD_D6.3_P2.5_C12345
ECAP_100U_25V_SMD_D6.3H5.4_C12345
```

---

## 5. Voltage Rating Rules

Use the rated voltage directly.

Examples:

```text
6V3
10V
16V
25V
35V
50V
63V
100V
```

Use `6V3` instead of `6.3V` if decimal points are avoided in component names.

Examples:

```text
ECAP_100U_6V3_SMD_D5.0H5.4_C12345
ECAP_47U_50V_RAD_D6.3_P2.5_C12345
```

---

## 6. Mounting Style Rules

| Mounting style | Field |
|---|---:|
| Surface mount | `SMD` |
| Radial through-hole | `RAD` |
| Axial through-hole | `AXIAL` |
| Snap-in | `SNAPIN` |
| Screw terminal | `SCREW` |

Examples:

```text
ECAP_100U_25V_SMD_D6.3H5.4_C12345
ECAP_470U_35V_RAD_D10.0_P5.0_C12345
ECAP_4700U_63V_SNAPIN_D25.0_P10.0_C12345
```

---

## 7. Size Field Rules

### 7.1 SMD electrolytic capacitors

Use diameter and height:

```text
D<diameter>H<height>
```

Examples:

```text
D4.0H5.4
D5.0H5.4
D6.3H5.4
D6.3H7.7
D8.0H10.2
```

Full examples:

```text
ECAP_10U_16V_SMD_D4.0H5.4_C12345
ECAP_100U_25V_SMD_D6.3H5.4_C12345
```

### 7.2 Radial through-hole electrolytic capacitors

Use body diameter and lead pitch:

```text
D<diameter>_P<pitch>
```

Examples:

```text
D5.0_P2.0
D6.3_P2.5
D8.0_P3.5
D10.0_P5.0
```

Full examples:

```text
ECAP_47U_25V_RAD_D5.0_P2.0_C12345
ECAP_470U_35V_RAD_D10.0_P5.0_C12345
```

Optional: include height for tall radial capacitors if mechanical clearance matters:

```text
ECAP_470U_35V_RAD_D10.0H16.0_P5.0_C12345
```

### 7.3 Tantalum capacitors

Use case size if that is how the part is normally specified:

```text
TANT_10U_16V_CASEA_C12345
TANT_47U_10V_CASEB_C12345
TANT_100U_6V3_CASED_C12345
```

---

## 8. Optional Feature Fields

Only include optional features when they are important for selection or not obvious from the base parameters.

Common feature tags:

| Feature | Tag |
|---|---:|
| Low ESR | `LOWESR` |
| High ripple current | `HIGHRIPPLE` |
| Long life | `LONGLIFE` |
| 105 °C rating | `105C` |
| 125 °C rating | `125C` |
| Automotive grade | `AECQ200` |
| Audio grade | `AUDIO` |
| Polymer | Prefer prefix `PCAP` instead |

Examples:

```text
ECAP_220U_16V_LOWESR_RAD_D8.0_P3.5_C12345
ECAP_100U_35V_105C_SMD_D6.3H7.7_C12345
ECAP_470U_25V_LONGLIFE_RAD_D10.0H20.0_P5.0_C12345
```

Do not overload the name with every datasheet parameter. Keep detailed parameters in component fields.

---

## 9. LCSC Number Rule

Append the LCSC part number at the end to guarantee uniqueness:

```text
_Cxxxxx
```

Examples:

```text
ECAP_100U_25V_SMD_D6.3H5.4_C12345
ECAP_100U_25V_SMD_D6.3H5.4_C67890
```

This allows two parts with the same electrical and mechanical description but different manufacturer, series, ESR, life, or stock source to coexist without name collision.

---

## 10. Recommended Component Parameters

The component name should be readable, but the full component identity should live in parameters.

Recommended parameters:

```text
Type = Aluminum Electrolytic Capacitor
Value = 100uF
Voltage Rating = 25V
Tolerance = ±20%
Mounting = SMD
Package / Size = D6.3H5.4
Operating Temperature = -40°C to +105°C
Lifetime = 2000 h @ 105°C
ESR = <datasheet value if available>
Ripple Current = <datasheet value if available>
Manufacturer = <actual manufacturer>
MPN = <manufacturer part number>
Supplier = LCSC
Supplier Part Number = Cxxxxx
LCSC Part Number = Cxxxxx
```

---

## 11. Separation from Symbol and Footprint Names

Do not use the exact component name for reusable schematic symbols.

Recommended separation:

| Library object | Example | Purpose |
|---|---|---|
| Component name | `ECAP_100U_25V_SMD_D6.3H5.4_C12345` | Exact purchasable component |
| Schematic symbol | `CAP_POL` | Generic polarized capacitor symbol |
| PCB footprint | `ECAP_SMD_D6.3H5.4` | Land pattern / physical package |
| 3D model | `ECAP_SMD_D6.3H5.4` | Physical body model |

---

## 12. Naming Examples

| Part description | Component name |
|---|---|
| 10 uF, 16 V, SMD aluminum electrolytic, 4.0 x 5.4 mm | `ECAP_10U_16V_SMD_D4.0H5.4_Cxxxxx` |
| 100 uF, 25 V, SMD aluminum electrolytic, 6.3 x 5.4 mm | `ECAP_100U_25V_SMD_D6.3H5.4_Cxxxxx` |
| 470 uF, 35 V, radial aluminum electrolytic, 10 mm diameter, 5 mm pitch | `ECAP_470U_35V_RAD_D10.0_P5.0_Cxxxxx` |
| 220 uF, 16 V, low-ESR radial aluminum electrolytic | `ECAP_220U_16V_LOWESR_RAD_D8.0_P3.5_Cxxxxx` |
| 100 uF, 16 V, SMD polymer capacitor | `PCAP_100U_16V_SMD_D6.3H5.8_Cxxxxx` |
| 47 uF, 10 V, tantalum case B | `TANT_47U_10V_CASEB_Cxxxxx` |

---

## 13. Final Recommendation

For normal aluminum electrolytic capacitors, use:

```text
ECAP_<VALUE>_<VOLTAGE>_<MOUNT>_<SIZE>_<LCSC>
```

For example:

```text
ECAP_100U_25V_SMD_D6.3H5.4_C12345
ECAP_470U_35V_RAD_D10.0_P5.0_C12345
```

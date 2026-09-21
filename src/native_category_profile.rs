//! Closed BuiltInCategory vocabularies for the BIM delivery product.
//!
//! These values are a caller convenience over the native numeric category
//! contract, not a decoder fallback.  They were transcribed from Autodesk's
//! documented `BuiltInCategory` enum for Revit 2025.3 and checked unchanged
//! against the 2027 enum on 2026-09-16.  Unknown labels deliberately fail.

use anyhow::{Result, ensure};
use std::collections::BTreeSet;

pub const VOCABULARY_VERSION: &str = "revit-2025.3-2027-bim-product-v1";

const CATEGORIES: &[(&str, i64)] = &[
    ("OST_CableTray", -2_008_130),
    ("OST_CableTrayFitting", -2_008_126),
    ("OST_Casework", -2_001_000),
    ("OST_Ceilings", -2_000_038),
    ("OST_Columns", -2_000_100),
    ("OST_CommunicationDevices", -2_008_081),
    ("OST_Conduit", -2_008_132),
    ("OST_ConduitFitting", -2_008_128),
    ("OST_CurtainWallMullions", -2_000_171),
    ("OST_CurtainWallPanels", -2_000_170),
    ("OST_DataDevices", -2_008_083),
    ("OST_Doors", -2_000_023),
    ("OST_DuctAccessory", -2_008_016),
    ("OST_DuctCurves", -2_008_000),
    ("OST_DuctFitting", -2_008_010),
    ("OST_DuctInsulations", -2_008_123),
    ("OST_DuctSystem", -2_008_015),
    ("OST_DuctTerminal", -2_008_013),
    ("OST_ElectricalEquipment", -2_001_040),
    ("OST_ElectricalFixtures", -2_001_060),
    ("OST_FireAlarmDevices", -2_008_085),
    ("OST_FlexDuctCurves", -2_008_020),
    ("OST_FlexPipeCurves", -2_008_050),
    ("OST_Floors", -2_000_032),
    ("OST_Furniture", -2_000_080),
    ("OST_FurnitureSystems", -2_001_100),
    ("OST_GenericModel", -2_000_151),
    ("OST_Grids", -2_000_220),
    ("OST_Levels", -2_000_240),
    ("OST_LightingDevices", -2_008_087),
    ("OST_LightingFixtures", -2_001_120),
    ("OST_MEPSpaces", -2_003_600),
    ("OST_MechanicalEquipment", -2_001_140),
    ("OST_NurseCallDevices", -2_008_077),
    ("OST_PipeAccessory", -2_008_055),
    ("OST_PipeCurves", -2_008_044),
    ("OST_PipeFitting", -2_008_049),
    ("OST_PipeInsulations", -2_008_122),
    ("OST_PipingSystem", -2_008_043),
    ("OST_PlumbingFixtures", -2_001_160),
    ("OST_Railings", -2_000_175),
    ("OST_Ramps", -2_000_180),
    ("OST_Roofs", -2_000_035),
    ("OST_Rooms", -2_000_160),
    ("OST_SecurityDevices", -2_008_079),
    ("OST_SpecialityEquipment", -2_001_350),
    ("OST_Sprinklers", -2_008_099),
    ("OST_Stairs", -2_000_120),
    ("OST_StairsLandings", -2_000_920),
    ("OST_StairsRuns", -2_000_919),
    ("OST_StructuralColumns", -2_001_330),
    ("OST_StructuralFoundation", -2_001_300),
    ("OST_StructuralFraming", -2_001_320),
    ("OST_TelephoneDevices", -2_008_075),
    ("OST_Walls", -2_000_011),
    ("OST_Windows", -2_000_014),
];

const ARCH_BUL: &[&str] = &[
    "OST_Walls",
    "OST_Floors",
    "OST_Roofs",
    "OST_Ceilings",
    "OST_Doors",
    "OST_Windows",
    "OST_Rooms",
    "OST_Stairs",
    "OST_StairsRuns",
    "OST_StairsLandings",
    "OST_Railings",
    "OST_Ramps",
    "OST_Columns",
    "OST_StructuralColumns",
    "OST_StructuralFraming",
    "OST_StructuralFoundation",
    "OST_CurtainWallPanels",
    "OST_CurtainWallMullions",
    "OST_Furniture",
    "OST_FurnitureSystems",
    "OST_Casework",
    "OST_GenericModel",
    "OST_SpecialityEquipment",
    "OST_PlumbingFixtures",
    "OST_LightingFixtures",
    "OST_MechanicalEquipment",
    "OST_ElectricalEquipment",
    "OST_Levels",
    "OST_Grids",
];

const MEP_BUL: &[&str] = &[
    "OST_MechanicalEquipment",
    "OST_ElectricalEquipment",
    "OST_PipeCurves",
    "OST_PipeFitting",
    "OST_PipeAccessory",
    "OST_PipeInsulations",
    "OST_FlexPipeCurves",
    "OST_DuctCurves",
    "OST_DuctFitting",
    "OST_DuctAccessory",
    "OST_DuctInsulations",
    "OST_FlexDuctCurves",
    "OST_DuctTerminal",
    "OST_CableTray",
    "OST_CableTrayFitting",
    "OST_Conduit",
    "OST_ConduitFitting",
    "OST_ElectricalFixtures",
    "OST_LightingFixtures",
    "OST_LightingDevices",
    "OST_PlumbingFixtures",
    "OST_Sprinklers",
    "OST_FireAlarmDevices",
    "OST_DataDevices",
    "OST_CommunicationDevices",
    "OST_SecurityDevices",
    "OST_TelephoneDevices",
    "OST_NurseCallDevices",
    "OST_GenericModel",
    "OST_SpecialityEquipment",
    "OST_Levels",
    "OST_PipingSystem",
    "OST_DuctSystem",
    "OST_MEPSpaces",
];

// Classes whose category is structural in the serialized owner kind rather
// than stored on the owner, its FamilySymbol, or its Family. These mappings
// are intentionally small and evidence-bound: MEP classes were checked against
// the original MEP BUL native/API probe, while `SWall` and `Floor` were checked
// on three exact ElementId/UniqueId-continuous ARCH owners each against the
// Revit 2027 API upgrade-copy witnesses `arch-category-witness-20260921-v1`
// and `arch-category-witness-20260921-v2`. The latter covers Ceiling, Grid,
// ProfileRoof, StairsElement, StairsRun, and StairsLanding. `ContFooting` was
// checked on all seven ARCH BUL `OST_StructuralFoundation` reference owners,
// each with a complete native graph. The type-owner classes below are the
// corresponding serialized definition owners for those same native families;
// their category is a structural class fact, not a caption/type-name
// heuristic. The 2026-09-21 ARCH missing-owner probe recovered these exact
// classes for the omitted type rows. Curtain panels are `FamilyInstance`
// owners and are intentionally excluded until their precise native type
// relationship is recovered. Unknown classes remain unresolved. `RoomElem` is
// deliberately excluded: its category is carried by the saved room/space
// scheme fields and is resolved by `native_delivery` from those fields, never
// from the shared serialized class name.
const OWNER_CLASS_CATEGORIES: &[(&str, i64)] = &[
    ("CableTray", -2_008_130),
    ("RbsCableTrayCurve", -2_008_130),
    ("RbsConduitCurve", -2_008_132),
    ("RbsDuctCurve", -2_008_000),
    ("RbsDuctInsulation", -2_008_123),
    ("RbsFlexDuctCurve", -2_008_020),
    ("RbsFlexPipeCurve", -2_008_050),
    ("RbsHvacSystem", -2_008_015),
    ("RbsHvacSystemType", -2_008_015),
    ("RbsPipeCurve", -2_008_044),
    ("RbsPipeInsulation", -2_008_122),
    ("RbsPipingSystem", -2_008_043),
    ("RbsPipingSystemType", -2_008_043),
    ("Level", -2_000_240),
    ("LevelAttributes", -2_000_240),
    ("SWall", -2_000_011),
    ("BasicWallType", -2_000_011),
    ("NewCurtainWallType", -2_000_011),
    ("Floor", -2_000_032),
    ("FloorAttributes", -2_000_032),
    ("Ceiling", -2_000_038),
    ("CompoundCeilingType", -2_000_038),
    ("Grid", -2_000_220),
    ("GridAttributes", -2_000_220),
    ("ProfileRoof", -2_000_035),
    ("RoofAttributes", -2_000_035),
    ("CurtainRoofAttributes", -2_000_035),
    ("StairsElement", -2_000_120),
    ("StairsType", -2_000_120),
    ("StairsAttributes", -2_000_120),
    ("StairsRun", -2_000_919),
    ("StairsRunType", -2_000_919),
    ("StairsLanding", -2_000_920),
    ("StairsLandingType", -2_000_920),
    ("ContFooting", -2_001_300),
    ("ContFootingType", -2_001_300),
    ("RampAttributes", -2_000_180),
    ("Text3dAttrSymbol", -2_000_151),
];

pub fn category_id(label: &str) -> Option<i64> {
    CATEGORIES
        .iter()
        .find_map(|(candidate, id)| (*candidate == label).then_some(*id))
}

pub fn category_label(id: i64) -> Option<&'static str> {
    CATEGORIES
        .iter()
        .find_map(|(label, candidate)| (*candidate == id).then_some(*label))
}

/// Checked category identity for a small set of serialized MEP owner classes
/// that do not store or inherit a category reference.  This is only for the
/// category-selection index; final delivery still retains the class and
/// source evidence on each owner.
pub fn owner_class_category_id(class_name: &str) -> Option<i64> {
    OWNER_CLASS_CATEGORIES
        .iter()
        .find_map(|(candidate, id)| (*candidate == class_name).then_some(*id))
}

pub fn profile_names() -> &'static [&'static str] {
    &["arch-bul-v1", "mep-bul-v1"]
}

pub fn profile_labels(name: &str) -> Result<&'static [&'static str]> {
    match name {
        "arch-bul-v1" => Ok(ARCH_BUL),
        "mep-bul-v1" => Ok(MEP_BUL),
        _ => anyhow::bail!(
            "unknown native category profile {name:?}; supported: {}",
            profile_names().join(", ")
        ),
    }
}

pub fn resolve_labels(labels: impl IntoIterator<Item = impl AsRef<str>>) -> Result<BTreeSet<i64>> {
    let mut ids = BTreeSet::new();
    for label in labels {
        let label = label.as_ref();
        let Some(id) = category_id(label) else {
            anyhow::bail!(
                "unknown native BuiltInCategory label {label:?} in vocabulary {VOCABULARY_VERSION}"
            );
        };
        ensure!(
            ids.insert(id),
            "duplicate native BuiltInCategory label {label:?}"
        );
    }
    Ok(ids)
}

pub fn resolve_profile(name: &str) -> Result<BTreeSet<i64>> {
    resolve_labels(profile_labels(name)?.iter().copied())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bul_profiles_are_closed_and_resolve_to_unique_native_ids() {
        assert_eq!(
            resolve_profile("arch-bul-v1").unwrap().len(),
            ARCH_BUL.len()
        );
        assert_eq!(resolve_profile("mep-bul-v1").unwrap().len(), MEP_BUL.len());
        assert_eq!(category_label(-2_008_044), Some("OST_PipeCurves"));
        assert_eq!(owner_class_category_id("RbsPipeCurve"), Some(-2_008_044));
        assert_eq!(owner_class_category_id("SWall"), Some(-2_000_011));
        assert_eq!(owner_class_category_id("Floor"), Some(-2_000_032));
        assert_eq!(owner_class_category_id("Ceiling"), Some(-2_000_038));
        assert_eq!(owner_class_category_id("Grid"), Some(-2_000_220));
        assert_eq!(owner_class_category_id("ProfileRoof"), Some(-2_000_035));
        assert_eq!(owner_class_category_id("StairsElement"), Some(-2_000_120));
        assert_eq!(owner_class_category_id("StairsRun"), Some(-2_000_919));
        assert_eq!(owner_class_category_id("StairsLanding"), Some(-2_000_920));
        assert_eq!(owner_class_category_id("ContFooting"), Some(-2_001_300));
        for (class_name, category_id) in [
            ("LevelAttributes", -2_000_240),
            ("BasicWallType", -2_000_011),
            ("NewCurtainWallType", -2_000_011),
            ("FloorAttributes", -2_000_032),
            ("CompoundCeilingType", -2_000_038),
            ("GridAttributes", -2_000_220),
            ("RoofAttributes", -2_000_035),
            ("CurtainRoofAttributes", -2_000_035),
            ("StairsType", -2_000_120),
            ("StairsAttributes", -2_000_120),
            ("StairsRunType", -2_000_919),
            ("StairsLandingType", -2_000_920),
            ("ContFootingType", -2_001_300),
            ("RampAttributes", -2_000_180),
            ("Text3dAttrSymbol", -2_000_151),
        ] {
            assert_eq!(owner_class_category_id(class_name), Some(category_id));
        }
        assert!(owner_class_category_id("RoomElem").is_none());
        assert!(owner_class_category_id("UnqualifiedNativeClass").is_none());
        assert!(resolve_profile("not-a-profile").is_err());
        assert!(resolve_labels(["OST_NotARevitCategory"]).is_err());
    }
}

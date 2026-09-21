# Native delivery coverage gaps and priorities

Status: active qualification plan, 2026-09-16.

This is a delivery plan for the product use case: turn the ARCH BUL and MEP
BUL RVT files into 3D Tiles/GLB plus rich, queryable BIM attributes.  It is
not a claim of general Revit API or IFC-converter equivalence.

## What the two readers establish

| Capability needed by the tiles product | ODA/DDC reference export | Native `rvt-rs` delivery |
| --- | --- | --- |
| Target physical-element inventory | Present: 83,141 MEP records; ARCH reference available | Not yet qualified: channel-102 discovery includes document internals as well as physical assets |
| Stable element identity | `element_id`, `unique_id`, type and category fields | Present: native identity/provenance, namespace and revision contract |
| Raw parameter values | Present, including instance/type parameters | Present where decoded, with raw storage/value/units provenance; field-by-field parity not yet measured |
| ODA-style category label | Present (`OST_*`) | Numeric BuiltInCategory recovered when stated; display-name mapping intentionally not guessed |
| Family/type/level/host/workset fields | Present in reference | Partial projection; needs field-level comparison on both models |
| Saved authored geometry | Present in ODA render path | Present for a supported subset; explicit refusal/partial states retained |
| Fine authored representations | Present in ODA render path | Available for tested saved graphics, but level 3 must be used when level 1/2 has no authored surface |
| Geometry fidelity against reference | Available comparison oracle | Not yet measured at mesh/topology level |
| Face materials and scalar appearance | Present in reference/render path | Supported subset with provenance; broad-model qualification remains open |
| Texture resources and UVs | Present when supplied/resolved | Partial and deliberately explicit; unqualified mappings stay diagnostic |
| Family-symbol reuse and instance placement | Present | Supported; shared symbols are retained while completed instance roots are released |
| 3D Tiles + binary GLB output | Existing ODA downstream path | Present: deterministic tileset, GLB-only canonical geometry, feature association and exact accessor deduplication |
| Render LOD hierarchy | Available downstream concern | Not implemented: current leaf geometry is exact; authored detail level is not LOD |
| Linked-model resolution / federation | ODA can expose links | Not supported; link occurrences are preserved only as unresolved references |
| 2D annotations, sheets and view-specific graphics | ODA may expose them | Out of scope for the current 3D-tiles product |

“Present” above means a concrete implementation exists, not that it has passed
the two-model qualification gate.  The ODA column is an oracle for the checked
exports, not a statement about every ODA SDK feature.

## Evidence already obtained

* The MEP ODA/DDC export contains 83,141 physical-element rows.  The ARCH
  export is available for the same comparison method.
* The initial low-detail MEP inventory run has shown that a raw native document
	population is much larger than the ODA physical-element export. At the
	2026-09-16 diagnostic snapshot it had emitted 668,000 records, including
	616,520 metadata-only records, 10.23 GB of element tables and 9.20 GB of
	GLBs; the checked MEP ODA physical population is 83,141 records. This is an
	inclusion-policy mismatch, not evidence that ODA elements are absent or that
	JSON meshes were duplicated.
* An Extensible Storage catalog omission previously caused 891 matching MEP
  `FamilyInstance` rows to fail current-graph decoding in an incomplete
  baseline.  Passing the document catalog to both metadata and saved-graphics
  extraction removes that known root cause in the targeted regression set.
* The same regression proves an important distinction: eleven owners have a
  valid decoded saved-graphics graph but no retained surface at authored detail
  level 1; they are not metadata-only.  At level 3 the sampled owners render,
  although one symbol expands to an unexpectedly large mesh and requires
  source-level tessellation investigation.
* The default delivery profile has one canonical binary geometry form (GLB),
  exact payload deduplication, and no JSON mesh/instance representation. Its
  shard path opens/indexes the RVT once, reuses document definitions and the ES
  catalog, and now releases completed instance-graphics roots. Document-level
  parameter definitions and custom parameter/category bindings are also pooled
  into once-per-package catalogs rather than repeated in every element or type
  row.

## Priority order and acceptance gates

## Remaining coverage map

| Gap | Why it blocks a replacement claim | Native path already in place | Priority / next proof |
| --- | --- | --- | --- |
| Physical-element selection | A raw channel-102 walk contains document internals, while the ODA exports are an intentional physical/category population. Comparing those populations directly would overstate both work and misses. | Validated current-record index, direct recovered category IDs, provenance, and one-reader sharding. | **P0.** Add a closed, versioned `OST_*` category profile and selection receipt. Select during a single metadata pass; reuse those decoded records for delivery. |
| Category labels | Numeric category IDs are not yet a complete `OST_*` vocabulary, so callers cannot express the proven ODA profile natively. | Numeric IDs are fail-closed and conflicts are rejected. | **P0.** Publish mappings only from the documented Revit BuiltInCategory enum; leave unknown values numeric. |
| Two-model inventory evidence | We have not yet run the corrected bounded path against the same selected ARCH and MEP populations. | ODA identity/category audit and native metadata-inventory audit scripts. | **P0.** One corrected run per source file, then account for every reference ID and every native exclusion. |
| Authored geometry coverage and fidelity | A successfully emitted GLB does not prove its meshes, transforms, or bounds match the reference. Fine authored detail can also expose pathological tessellation. | Saved-scene traversal, GLB-only 3D Tiles output, feature/attribute joins, explicit diagnostics, and compact per-owner renderable AABBs in meters. | **P1.** Category-stratified bounds/topology/material comparison; the audit now compares ODA ft AABBs to native meter AABBs without claiming mesh parity. Fix the high-expansion trim/tessellation source before any simplification policy. |
| Rich metadata semantics | Stored values are projected, but type inheritance, evaluated Revit API values, display units, and relationship fields are not all proven equivalent. | Parameter-definition/binding catalogs and raw value provenance; field-level audit. | **P1.** Establish required fields and compare per category; classify every difference as stored-value, inheritance/evaluation, unsupported, or defect. |
| Material and UV qualification | Partial material/UV support can make a tile render incorrectly even when geometry is present. | Face-material subset and explicit unresolved-resource diagnostics. | **P2.** Measure outcomes by category and require a qualified binding or diagnostic for every primitive. |
| Linked models / sheets / annotations | These are capabilities outside the source-RVT physical 3D tile product. | Unresolved links are preserved as references. | **P3 / out of scope.** Do not expand until the product requires federation or 2D deliverables. |

The recommended order is therefore: **selection contract and category
vocabulary → one-pass qualified inventory on MEP and ARCH → geometry and
metadata comparison → material/UV qualification**. This ordering avoids a
large, broad reread that would later be discarded when the category contract is
applied.

### P0 — qualify the selection contract and bounded execution

Why first: a percentage is meaningless until native and ODA are comparing the
same population.  This also prevents a whole-file rerun from silently storing
or processing non-deliverable document records.

1. Define a fail-closed physical-element selection policy, expressed in terms
   of native provenance/class/category rather than a hard-coded list of known
   MEP categories.  Retain excluded records only as aggregate audit counts.
2. Run the revised one-reader, bounded-cache executable once across MEP, then
   ARCH, with the same selected population contract.  Record peak RSS, elapsed
   time, GLB bytes and table bytes per selected element.
3. Audit exact ODA element-ID coverage and category agreement for both models.
   Any missing selected ODA ID must have a classified reason; any extra native
   selected ID must be an intentionally documented inclusion. The root
   category-selection receipt must also distinguish an ODA row outside the
   requested profile from a requested row missing from native delivery.

**Exit gate:** 100% of ODA reference IDs are either delivered or have a
specific, non-generic refusal; no duplicate JSON geometry; no repeated
document-wide parse; measured RSS remains bounded as shards advance.

`spatiallogic/tools/bim/research/run_native_delivery_qualification.py` is the
required execution wrapper for this gate. It invokes the native package binary
exactly once with a checked BUL profile, then runs only read-only coverage and
metadata audits and writes elapsed time, peak parser RSS, source/package bytes,
an explicit GLB/metadata/catalog byte breakdown, and both audit paths into a
separate receipt directory. It also requires the
original ODA export config and fails unless the native receipt's checked labels
exactly equal that config's `OST_*` list; it does not trust normalized source
provenance, because type-only source values can otherwise pollute it.
The wrapper also fails the qualification rather than merely reporting it when
any requested ODA owner is absent, a matched owner lacks a native category ID
or checked label, native/ODA category labels disagree, or the owner-metadata
projection is absent.
It additionally validates published GLB structural JSON against each shard's
element rows: retained renderable owners must join one-to-one to GLB attribute
rows and nodes, and every primitive must carry the same stable owner key. This
is artifact integrity evidence, not a mesh-parity claim. The same check also
requires triangle primitives with nonempty, cardinality-consistent POSITION,
NORMAL and index accessors backed by GLB binary buffer views whose byte ranges
fit the declared GLB binary chunk.

### P1 — make geometry coverage trustworthy

Why next: a `complete_supported_subset` status proves only that the currently
supported path rendered, not that it matches the authored element.

1. Add a representative, category-stratified geometry comparator: bounds,
   triangle/primitive counts, transforms, material assignments and diagnostic
   state.  Use ODA output/render-derived metrics as the oracle where available.
2. Fix the pathological saved-face tessellation at its sampling/trim source.
   Do not hide it with blind mesh decimation: it can erase geometry needed for
   a faithful BIM tile.
3. Choose a delivery policy for authored detail.  Fine (level 3) is the
   fidelity baseline; any viewer LOD must be a separate post-extraction mesh
   simplification with a measured error bound and preserved feature IDs.
4. Classify remaining missing graphics into: no saved graphics, valid but
   nonrenderable at selected authored detail, unsupported representation, or
   resource-limit refusal.

**Exit gate:** both models have a published category-stratified comparison;
every selected nonrenderable owner is classified; sampled geometry has agreed
bounds/transforms and no unexplained high-expansion faces.

### P1 — measure metadata and relationship parity

Why in parallel with geometry: metadata is the other half of the product, and
the reference exports already provide a concrete oracle.

1. Compare native rows to ODA rows by element ID for identity, category,
   family/type, level, workset, host, system/circuit fields, and parameter
   names/types/values.
   The `audit_native_metadata_coverage.py` companion reports owner-stored field
   presence/equality through the shared parameter catalogs, and explicitly
   labels type-inheritance/API-evaluation as outside that claim.
2. Publish a versioned BuiltInCategory-to-`OST_*` mapping.  Keep unknown IDs
   numeric and explicit rather than fabricating names.
3. Verify type/instance inheritance and units/specification handling with
   exact value tolerances, not stringified display values.

**Exit gate:** a field-level diff report for ARCH and MEP identifies each
missing or divergent field by cause; required product attributes have an
agreed completeness threshold and no silent coercion.

### P2 — qualify materials, UVs and packaging

Why after primary geometry: missing textures should not block usable geometry,
but material/UV mismatches must be visible to consumers.

1. Stratify material, bitmap-reference and UV mapping outcomes on both models.
2. Verify one GLB payload per shard/package and stable feature-to-attribute
   joins after batching/deduplication.
3. Add packaging rules only for caller-authorized texture retrieval; never
   filesystem-scan for assets.

**Exit gate:** every render primitive has either an emitted qualified material
/ UV binding or an explicit diagnostic, and package byte accounting shows no
parallel mesh representation.

### P3 — deferred unless product scope expands

* Linked RVT resolution and federated coordinate registration.
* Drawings, sheets, annotation graphics and schedules.
* General Revit write/round-trip behavior and converter-grade IFC.

These are real reader gaps, but none is necessary to replace ODA for the
current source-RVT-to-3D-Tiles-and-rich-attributes path.

## Current decision

Do not declare ODA supplanted yet.  The implementation is close enough to run
the product pipeline without JSON geometry duplication, but it still needs P0
qualification on both large files and P1 evidence for geometry and metadata.
The next productive run is the revised bounded-cache delivery path, not another
old-binary full scan or a second representation of the same mesh data.

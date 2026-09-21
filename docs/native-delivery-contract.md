# Native scene delivery contract

Status: implementation in progress. This contract describes additive export APIs.

The initial implementation is exposed by `rvt-native-package` and the
`native_delivery` module. It writes a manifest plus deterministic element
metadata in `elements.json`; element values and their provenance remain in the
element row, while document-scoped parameter definitions are emitted once in
`parameter-definitions.jsonl` and custom parameter/category bindings are
emitted once in `parameter-bindings.jsonl`; owner values, absence and global
parameter-association state remain local to the owner row. Both catalogs are
referenced by stable numeric parameter identity. The definition catalog carries
an explicit `definition_kind` so file-owned definitions and validated built-in
catalog definitions are never conflated.
The default `tiles` profile writes only the manifest, element attributes,
shared parameter-definition catalog, `tileset.json`, and binary GLB content.
It never serializes mesh
vertices, normals, or triangle indices as JSON. `rich-tiles` adds compact JSONL
domain tables for types, relationships, and materials plus spatial
context/boundaries; `audit` adds metrics and texture-mapping receipts.
The caller supplies `--document-namespace`; a source revision digest may be
supplied separately with the hidden operational `--source-sha256` option.
The CLI does not pre-hash the whole RVT because that would create an unneeded
second file pass before extraction. Output is published through a staging directory and an
atomic rename, and an existing output directory is rejected.
Optional profile tables are emitted when available; projection failures remain
manifest diagnostics and mark the package partial.

The manifest contains bounded, aggregate coverage diagnostics only. Owner-level
diagnostics belong to the relevant rich/audit row or audit receipt; they are
never copied into the manifest, so a large population of equivalent failures
cannot inflate the default tiles package.

`spatial-boundaries.json` contains the raw boundary
inventory plus a `qualified_space_volumes` list; a volume is emitted only when
an evaluated Finish loop and explicit level/height references resolve. Failed
space qualifications remain diagnostics.

When geometry instances are present, the package additionally writes
`tileset.json` and one exact `content.glb` per package/shard. Its element nodes
retain the attribute-row association while identical vertex/normal/index
payloads share one binary accessor set. Packages with no geometry omit the
tileset artifact rather than inventing an empty render tile; geometry-bearing
packages omit empty/unsupported owners from renderable geometry and retain
coverage diagnostics in the manifest (and in
`metrics.jsonl` for the `audit` profile) as explicit diagnostic receipts.
An owner whose saved graphics decoded but whose requested detail/visibility
selection retained no surface primitive is reported separately from an owner
with no graphics record or closure; it is not counted as metadata-only.

The delivery package joins current native records and saved graphics without
claiming that every current record is a physical asset. It supports glTF, tiled
delivery and downstream site-model consumers from the same element rows.

For a physical product profile, `--category-id` accepts one or more native
numeric Revit BuiltInCategory IDs. The one-reader path first builds the normal
document definition context, then decodes current metadata once to make a
fail-closed selection. It keeps only selected metadata/provenance and releases
the corresponding raw graph before saved-graphics extraction. The later shard
therefore consumes that exact prepared metadata rather than decoding selected
metadata a second time. Owners decoded to build document definitions, units,
family/category links, and parameter bindings are also excluded from the
category scan; the receipt records that reused population and asserts that it
plus the scanned population accounts for every current channel-102 owner. The
sharded root manifest carries aggregate
`category_selection` evidence: requested IDs, selected/excluded counts by
category, and explicit no-category/unsupported counts. `--category-id` cannot
be combined with explicit element `--ids`. Isolated workers are supported:
category metadata is selected exactly once in the parent process, then the
resulting native element IDs are passed explicitly to fresh workers; workers
never repeat category discovery. A versioned `OST_*` label
profile is a caller-facing layer over these numeric IDs; unknown categories
remain numeric and never match by guessed name. The checked vocabulary version
is `revit-2025.3-2027-bim-product-v1`; it contains the exact ODA export
category lists as `--category-profile arch-bul-v1` and
`--category-profile mep-bul-v1`. `--category OST_*` adds individual checked
labels, while `--category-id` remains available for a caller's separately
versioned extension. The profile vocabulary is based on the documented Revit
2025.3 and 2027 `BuiltInCategory` enum values, which were verified identical
for this target set. A known vocabulary ID is additionally emitted as
`category_label`; an unknown native ID is emitted only as `category_id`.

For broad populations, `rvt-native-package --shard-size N` writes a
`rvt-native-delivery-sharded-v2` directory. Its `manifest.json` indexes
deterministic current channel-102 id ranges, and each `shard-XXXXXX` child is
an ordinary delivery package with its selected-profile metadata tables, exact
leaf tiles, and GLBs. The default CLI path builds the physical index and
definition context once, then keeps one RVT reader open while each shard
decodes only the selected owners plus required saved graphics/material closure.
This avoids reopening the file and repeating document-wide discovery for every
shard. All decoded graphics, symbol closure, and material-resolution state
are released after each package. Shared symbols are re-decoded when needed by
a later shard so resident memory remains bounded by the active shard rather
than the document-wide selection. `--isolate-workers` opts into a fresh worker process and RVT reader per
shard when a hard RSS boundary is more important than reuse.

The CLI accepts `--shard-max-cost-bytes B`. When set,
IDs remain in deterministic order but are packed by validated channel-102 and
channel-103 stored-body estimates, optionally still capped by `--shard-size`.
Each top-level shard manifest records `estimated_cost_bytes`; referenced
symbol closure can exceed that estimate, so graph resource budgets remain the
authoritative safety boundary.

For files whose decoded graph or tessellation expansion is large relative to
stored-body size, `--shard-max-owners N` adds a hard selected-owner cap to the
planner. When both limits are present, the effective owner cap is the smaller
of `--shard-size` and `--shard-max-owners`; the one-reader delivery path is
unchanged.
With `--isolate-workers`, `--worker-max-rss-bytes B` adds a hard per-worker RSS
guard. A worker that crosses the bound is terminated and the sharded command
returns an explicit failure; completed sibling shards remain in staging for
diagnosis, but no incomplete aggregate package is published.

Document-wide parameter definitions, binding context, units and builtin
catalog provenance are decoded once and reused across the default one-reader
shard run. Shard metadata therefore retains definition-resolved attributes
without repeating document-wide definition work or rematerializing the
extensible storage catalog for every shard.

For a sharded package, `parameter-definitions.jsonl` is written once at the
aggregate root. Each child manifest references it with
`parameter_definitions_uri: "../parameter-definitions.jsonl"` and does not
copy the catalog. `parameter-bindings.jsonl` follows the same rule through
`parameter_bindings_uri`. A standalone package owns both catalogs locally.
This makes the same document context addressable without multiplying it by the
shard count.

The aggregate root must declare those exact local URI values and every shard
URI must name one unique direct child (no absolute or parent-traversal path).
When `--isolate-workers` is used, a worker's transient catalogs are required
to be byte-identical to the parent catalogs before they are deleted and
replaced by root references. A mismatch is a failed package, not a
best-effort merge: parameter identity is document-scoped provenance.

The required qualification wrapper verifies that layout from the published
root: exactly one copy of each document catalog, no child catalog copies, and
GLB/metadata/catalog/tileset/manifest bytes reported separately. The package
integrity audit also validates every declared rich semantic table (`types`,
`relationships`, `materials`, spatial context/boundaries, room connections and
network) with its declared JSON or JSONL encoding. It rejects unknown or
duplicate table declarations, malformed catalog references, and any parallel
JSON mesh artifact.

The same one-time Extensible Storage schema catalog is passed to saved-graphics
closure. A schema-defined payload cannot make an otherwise decodable
FamilyInstance graphics graph fail merely because the selected shard did not
reconstruct the document catalog itself.

When at least one shard has renderable geometry, the sharded root also writes
`tileset.json`. Its exact root is an external-child 3D Tiles 1.1 tileset whose
children point to `shard-XXXXXX/tileset.json`; empty-only shards remain indexed
in the manifest but are not fabricated as render tiles. The aggregate root is
an addressable viewer entry point, while element metadata remains shard-local
in each `elements.json` and is reached through the manifest and child packages.

Sharded manifests may include an optional `telemetry` object on each shard
entry. Version `1` records `elapsed_ms`, `renderable_vertices`,
`renderable_triangles`, `glb_bytes`, `package_bytes`, and optional
`peak_rss_bytes`. Older manifests omit this object; consumers should treat it
as diagnostic data rather than delivery identity or coverage authority.

## Identity and tables

Require a caller-supplied document namespace. Keep it separate from the source
revision SHA-256. An element key combines that namespace with its native unique
identity; numeric element IDs alone are not globally unique. Duplicate or
conflicting identities are errors. Revision changes do not silently change a
namespace. Link occurrences additionally retain their placement identity.

Produce deterministic element rows and shared document parameter-definition
and parameter-binding catalogs, plus, when selected by profile, type, material,
relationship, spatial, and audit tables. Element rows retain parameter identity, storage type, raw
value, units/specification, source owner and instance/type
distinction. Empty, absent, unresolved and unsupported values must not be
conflated. Keep metadata rows for metadata-only owners, with explicit join and
diagnostic status; omit non-renderable geometry rows rather than fabricating
geometry. Canonical geometry is represented only by GLB binary buffers; no
parallel JSON mesh-asset, mesh-use, or instance tables are written. Do not
repeat document-level parameter-definition or category-binding objects in each
element or type row.

When the saved graph states `m_categoryId`, element rows also retain its numeric
Revit BuiltInCategory identity. The package does not guess a human display name
for an unknown numeric category; downstream category naming is a separate,
versioned mapping concern.

## Geometry and placement

Mesh positions use double precision during extraction. Package positions are
local meters/Z-up, with a row-major affine instance matrix acting on column
vectors. A glTF adapter applies meters/Z-up to meters/Y-up once. Mesh bounds,
complete-object bounds and space bounds are distinct: partial geometry can only
establish bounds of the recovered subset.

Use a qualified native instance transform to recover reusable local geometry;
otherwise rebase around a deterministic local origin and state that original
instancing is unresolved. Deduplicate geometry assets by exact vertex, normal
and index payload inside the GLB emission path. Preserve transforms and normal
inverse-transpose behavior, including mirrored winding.

Document-to-site placement is optional explicit caller input, never inferred
from an absent shared-coordinate registration. Preserve its source/target frame
identifiers, direction and units. Missing linked files remain unresolved links.
Repeated link occurrences require distinct instance keys. No canonical world
asset or approved spatial registration is created by this library.

## Geometry coverage

Support qualified sampled trim regions with holes and islands. Ring order does
not determine hole status. Crossing, malformed, ambiguous and unbounded trims
require explicit diagnostics. Periodic normalization must preserve the selected
region, including periodic Hermite profile seams, and curved tessellation has
finite error/resource budgets. Shared edge
samples and source material ownership survive subdivision. A successful parser
call alone is not proof of closed solids or complete shape recovery.

`--detail-level` selects Revit's authored saved-graphics representation (1,
2, or 3); it is not a mesh LOD control. An owner can legitimately have no
surface representation at a lower authored detail and appear at level 3.
Delivery LOD simplification must therefore operate after qualified geometry
extraction, preserve the element feature association, and report its measured
geometric error rather than silently substituting a lower authored detail.

## Spaces and render resources

Space extents require qualified boundary loops and vertical references; a room
location or equipment bounds cannot substitute. Retain boundary kind, host/link
references, frame and unresolved vertical/planar conditions. Export bounds only
for the supported derivation, identifying the assumption about vertical sides.

Materials retain face assignment and scalar properties independently of texture
availability. Only explicitly supplied resources may be packaged. UV/resource
bindings require owner/face/material agreement; unsupported mappings stay
diagnostic. Direct instance-to-symbol saved graphics closure may carry
`instance_object_index` and composes symbol-local UV planes through the saved
instance transform. Qualified revolved mappings may invert saved `GArc`,
`GLine`, `GEllipse` or periodic/non-periodic `GHermiteSpline` profile parameters
with residual validation. Qualified tensor-product `HermiteSurf` grids retain
their saved parameter knots and point/tangent/mixed-derivative nodes for
geometry and UV inversion; periodic surface axes remain explicitly gated.
Saved placer mirror state is retained as provenance.
Texture mapping selection follows the explicit metadata selection
when supplied; otherwise it follows the native selected-id set. Never derive a
texture path by scanning arbitrary directories.
Saved material rows may also retain exact bitmap-reference strings from native
texture slots, with de-duplicated candidate paths, slot names and graph-object
provenance. These are references only: filesystem lookup, authorization,
copying and image embedding remain caller-owned and must be explicit.
When a qualified mapping evaluates every recovered vertex of a primitive, the
GLB adapter emits its `TEXCOORD_0` accessor; otherwise it preserves the
primitive and leaves the mapping in the side table with its diagnostic. Each
primitive's `extras.native_delivery.texture_mapping` records `status` as
`emitted`, `not_emitted`, or `not_available`, plus a reason when UVs were not
emitted. This makes omission explicit rather than requiring consumers to infer
it from the absence of an attribute.

## Outputs and validation

A package manifest indexes element/relationship tables and mesh resources.
`GeometryInstance` is the stable handoff for later glTF and 3D Tiles workers:
it carries a document-frame label, a local meter/Z-up origin, rebased meshes,
and saved render-material rows keyed by primitive. The glTF adapter applies the
single Z-up to Y-up conversion and does not rebase again.
The glTF adapter keeps stable feature IDs through batching and preserves them in
the element table, plus per-primitive mesh identity/provenance in glTF extras.
Spatial tiles retain those IDs; empty or unsupported owners are reported in
metrics rather than silently converted to empty renderables.
Render LOD simplification is separate from authored detail selection and must
declare measured error; an exact leaf-only tileset is not a simplified LOD tree.

Validation covers identity joins, duplicate/conflicting inputs, transforms and
large-coordinate precision, geometry topology/area/volume/normal checks where
applicable, missing resources, deterministic output and export/table round trips.
Completeness is reported per population and projection, never as a percentage of
the whole file format.

Rich and audit packages also emit `room-connections.json`. This is an
evidence-bearing projection of phase-qualified door adjacency computed from
explicit host topology, lifecycle state, room boundaries, and saved door
orientation. It reports unresolved doors and
`complete_room_connection_parity: false`; it never infers adjacency from
nearest-room geometry or claims Revit API parity.

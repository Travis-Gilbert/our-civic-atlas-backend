//! Spec-to-geometry massing realization.
//!
//! The footprint and massing are authoritative (Sanborn, parcel). This
//! module never regenerates them: it realizes the spec's existing `Mass`,
//! `Facade`, `OpeningGrid` and `Roof` as renderable geometry, one part per
//! spec part, so provenance stays addressable per part.
//!
//! Geometry convention (matches the deck.gl ScenegraphLayer contract and the
//! Blender archetype GLBs validated in Phase A.5): real-world meters, glTF
//! +Y up, ground plane at Y=0, footprint centered on the origin, front
//! facade facing +Z. The layer applies position and bearing per feature, so
//! the asset itself is unrotated.

use anyhow::{Context, Result};

use civic_atlas_types::civic_atlas::v1::{DimensionRange, Facade, ReconstructionSpec};

use crate::provenance::{classify_part, PartFlag};

/// Roof silhouette, normalized from the spec's free-string `roof_type`.
/// Mirrors the frontend's `RoofForm` ('flat' | 'gable' | 'hipped').
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoofForm {
    Flat,
    Gable,
    Hipped,
}

impl RoofForm {
    pub fn as_str(&self) -> &'static str {
        match self {
            RoofForm::Flat => "flat",
            RoofForm::Gable => "gable",
            RoofForm::Hipped => "hipped",
        }
    }
}

pub fn normalize_roof_form(roof_type: &str) -> RoofForm {
    let normalized = roof_type.to_ascii_lowercase();
    if normalized.contains("gable") {
        RoofForm::Gable
    } else if normalized.contains("hip") {
        RoofForm::Hipped
    } else {
        RoofForm::Flat
    }
}

/// What kind of spec part a geometry part realizes. Drives material
/// selection and node-tree addressing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PartKind {
    Wall,
    GroundFloor,
    Opening,
    Roof,
    Foundation,
    /// Projecting trim (cornice, parapet band). Kept distinct from Roof so
    /// roof-form geometry stays addressable on its own.
    Trim,
}

/// Building side in plan. Front faces +Z.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Front,
    Right,
    Back,
    Left,
}

impl Side {
    pub const ALL: [Side; 4] = [Side::Front, Side::Right, Side::Back, Side::Left];

    pub fn as_str(&self) -> &'static str {
        match self {
            Side::Front => "front",
            Side::Right => "right",
            Side::Back => "back",
            Side::Left => "left",
        }
    }
}

/// One renderable part: geometry plus the provenance flag and material it
/// presents with.
#[derive(Debug, Clone)]
pub struct MassingPart {
    /// Spec field path ("mass", "facades[0]", "roof", "ground_floor",
    /// "facades[0].openingGrids[0]"). Becomes the node-tree address.
    pub field_path: String,
    pub label: String,
    pub kind: PartKind,
    pub flag: PartFlag,
    /// Material name for dedup ("brick", "ghost-wall", ...).
    pub material_name: String,
    /// Linear-space RGBA base color.
    pub base_color: [f32; 4],
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    pub indices: Vec<u32>,
}

impl MassingPart {
    fn new(
        field_path: impl Into<String>,
        label: impl Into<String>,
        kind: PartKind,
        flag: PartFlag,
        material_name: impl Into<String>,
        base_color: [f32; 4],
    ) -> Self {
        Self {
            field_path: field_path.into(),
            label: label.into(),
            kind,
            flag,
            material_name: material_name.into(),
            base_color,
            positions: Vec::new(),
            normals: Vec::new(),
            indices: Vec::new(),
        }
    }

    fn is_empty(&self) -> bool {
        self.indices.is_empty()
    }

    /// Push a quad with the given outward normal. Vertices in CCW order
    /// seen from the normal side.
    fn push_quad(&mut self, corners: [[f32; 3]; 4], normal: [f32; 3]) {
        let base = self.positions.len() as u32;
        self.positions.extend_from_slice(&corners);
        self.normals.extend_from_slice(&[normal; 4]);
        self.indices
            .extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    }

    /// Push a triangle with the given outward normal, CCW from the normal
    /// side.
    fn push_triangle(&mut self, corners: [[f32; 3]; 3], normal: [f32; 3]) {
        let base = self.positions.len() as u32;
        self.positions.extend_from_slice(&corners);
        self.normals.extend_from_slice(&[normal; 3]);
        self.indices.extend_from_slice(&[base, base + 1, base + 2]);
    }
}

/// Plan dimensions resolved from the spec, in meters.
#[derive(Debug, Clone, Copy)]
pub struct Dims {
    pub width: f32,
    pub depth: f32,
    pub height: f32,
    pub stories: u32,
}

const METERS_PER_FOOT: f64 = 0.3048;
const DEFAULT_STORY_HEIGHT_M: f64 = 3.2;

fn range_value_m(range: Option<&DimensionRange>) -> Option<f64> {
    let range = range?;
    let value = match (range.min, range.max) {
        (Some(min), Some(max)) => (min + max) / 2.0,
        (Some(min), None) => min,
        (None, Some(max)) => max,
        (None, None) => return None,
    };
    if value <= 0.0 {
        return None;
    }
    let unit = range.unit.to_ascii_lowercase();
    if unit.starts_with("ft") || unit.contains("feet") || unit.contains("foot") {
        Some(value * METERS_PER_FOOT)
    } else {
        Some(value)
    }
}

/// Resolve building dimensions from the spec's mass. Width and depth fall
/// back to a modest residential footprint; height falls back to
/// stories * 3.2 m. The fallbacks only fire when the merged spec genuinely
/// lacks the dimension, which itself reads as inference through the mass
/// part's provenance flag.
pub fn dims_from_spec(spec: &ReconstructionSpec) -> Dims {
    let mass = spec.mass.as_ref();
    let stories = mass
        .map(|mass| mass.stories)
        .filter(|s| *s > 0)
        .unwrap_or(2);
    let height = mass
        .and_then(|mass| range_value_m(mass.height.as_ref()))
        .unwrap_or(stories as f64 * DEFAULT_STORY_HEIGHT_M);
    let width = mass
        .and_then(|mass| range_value_m(mass.width.as_ref()))
        .unwrap_or(10.0);
    let depth = mass
        .and_then(|mass| range_value_m(mass.depth.as_ref()))
        .unwrap_or(12.0);
    Dims {
        width: width as f32,
        depth: depth as f32,
        height: height as f32,
        stories,
    }
}

/// Wall material palette. Mirrors `material_color` in
/// `primitives/scripts/render_spec.py` so the synchronous massing render
/// and the Blender archetype refinement read as the same building.
pub fn wall_color(material: &str) -> (&'static str, [f32; 4]) {
    let key = material.to_ascii_lowercase();
    if key.contains("brick") {
        ("brick", [0.58, 0.18, 0.11, 1.0])
    } else if key.contains("stone") || key.contains("limestone") {
        ("stone", [0.72, 0.69, 0.62, 1.0])
    } else if key.contains("wood") || key.contains("clapboard") || key.contains("frame") {
        ("wood-frame", [0.78, 0.68, 0.52, 1.0])
    } else if key.contains("concrete") {
        ("concrete", [0.55, 0.55, 0.50, 1.0])
    } else if key.contains("fire") {
        ("fire-resistive", [0.60, 0.60, 0.58, 1.0])
    } else if key.contains("steel") || key.contains("metal") {
        ("metal", [0.42, 0.42, 0.42, 1.0])
    } else {
        ("masonry-unknown", [0.68, 0.66, 0.58, 1.0])
    }
}

pub fn roof_color(material: &str) -> (&'static str, [f32; 4]) {
    let key = material.to_ascii_lowercase();
    if key.contains("slate") {
        ("slate", [0.18, 0.20, 0.22, 1.0])
    } else if key.contains("copper") {
        ("copper", [0.32, 0.65, 0.55, 1.0])
    } else if key.contains("metal") || key.contains("tin") || key.contains("steel") {
        ("roof-metal", [0.45, 0.46, 0.48, 1.0])
    } else if key.contains("shingle") || key.contains("asphalt") {
        ("shingle", [0.25, 0.22, 0.20, 1.0])
    } else {
        ("roofing", [0.30, 0.28, 0.26, 1.0])
    }
}

const GLASS_COLOR: [f32; 4] = [0.27, 0.36, 0.40, 1.0];

/// The realized massing model: parts plus the resolved envelope.
#[derive(Debug, Clone)]
pub struct MassingModel {
    pub parts: Vec<MassingPart>,
    pub dims: Dims,
    pub roof_form: RoofForm,
}

struct SideGeometry {
    /// Outward normal.
    normal: [f32; 3],
    /// Half-extent of the building along this side's normal axis.
    offset: f32,
    /// Width of the wall face.
    width: f32,
}

fn side_geometry(side: Side, dims: &Dims) -> SideGeometry {
    match side {
        Side::Front => SideGeometry {
            normal: [0.0, 0.0, 1.0],
            offset: dims.depth / 2.0,
            width: dims.width,
        },
        Side::Back => SideGeometry {
            normal: [0.0, 0.0, -1.0],
            offset: dims.depth / 2.0,
            width: dims.width,
        },
        Side::Right => SideGeometry {
            normal: [1.0, 0.0, 0.0],
            offset: dims.width / 2.0,
            width: dims.depth,
        },
        Side::Left => SideGeometry {
            normal: [-1.0, 0.0, 0.0],
            offset: dims.width / 2.0,
            width: dims.depth,
        },
    }
}

/// A point on a wall face: `u` runs across the face (left-to-right seen from
/// outside), `y` is height, `out` is the outward offset from the wall plane.
fn wall_point(side: Side, geometry: &SideGeometry, u: f32, y: f32, out: f32) -> [f32; 3] {
    let plane = geometry.offset + out;
    match side {
        // Seen from +Z, u runs -X..+X.
        Side::Front => [u, y, plane],
        // Seen from -Z, u runs +X..-X (mirrored so CCW stays outward).
        Side::Back => [-u, y, -plane],
        // Seen from +X, u runs +Z..-Z.
        Side::Right => [plane, y, -u],
        // Seen from -X, u runs -Z..+Z.
        Side::Left => [-plane, y, u],
    }
}

fn push_wall_band(
    part: &mut MassingPart,
    side: Side,
    geometry: &SideGeometry,
    y_bottom: f32,
    y_top: f32,
) {
    let half = geometry.width / 2.0;
    part.push_quad(
        [
            wall_point(side, geometry, -half, y_bottom, 0.0),
            wall_point(side, geometry, half, y_bottom, 0.0),
            wall_point(side, geometry, half, y_top, 0.0),
            wall_point(side, geometry, -half, y_top, 0.0),
        ],
        geometry.normal,
    );
}

/// Emit a wall rectangle in (u, y) face coordinates at the wall plane.
/// No-op for degenerate spans so the punched-wall tessellation can pass
/// zero-width strips without producing junk triangles.
fn push_wall_rect(
    part: &mut MassingPart,
    side: Side,
    geometry: &SideGeometry,
    u0: f32,
    u1: f32,
    v0: f32,
    v1: f32,
) {
    if u1 - u0 <= 1e-3 || v1 - v0 <= 1e-3 {
        return;
    }
    part.push_quad(
        [
            wall_point(side, geometry, u0, v0, 0.0),
            wall_point(side, geometry, u1, v0, 0.0),
            wall_point(side, geometry, u1, v1, 0.0),
            wall_point(side, geometry, u0, v1, 0.0),
        ],
        geometry.normal,
    );
}

/// Emit a recessed opening: a glass pane set back into the wall by `reveal`,
/// plus four jamb faces (sill, head, two reveals) bridging the wall plane to
/// the glass plane. Without CSG, the punched-wall tessellation leaves the
/// hole and this fills it with depth, so windows read as openings (not flat
/// patches) at city zoom. Jambs are wall-colored thickness and go in the
/// wall part; the pane goes in the glazing part. Materials are double-sided,
/// so approximate jamb normals are acceptable.
fn push_recessed_opening(
    wall: &mut MassingPart,
    glazing: &mut MassingPart,
    side: Side,
    geometry: &SideGeometry,
    u0: f32,
    u1: f32,
    v0: f32,
    v1: f32,
    reveal: f32,
) {
    if u1 - u0 <= 1e-3 || v1 - v0 <= 1e-3 {
        return;
    }
    glazing.push_quad(
        [
            wall_point(side, geometry, u0, v0, -reveal),
            wall_point(side, geometry, u1, v0, -reveal),
            wall_point(side, geometry, u1, v1, -reveal),
            wall_point(side, geometry, u0, v1, -reveal),
        ],
        geometry.normal,
    );
    // Sill (bottom jamb).
    wall.push_quad(
        [
            wall_point(side, geometry, u0, v0, 0.0),
            wall_point(side, geometry, u1, v0, 0.0),
            wall_point(side, geometry, u1, v0, -reveal),
            wall_point(side, geometry, u0, v0, -reveal),
        ],
        geometry.normal,
    );
    // Head (top jamb).
    wall.push_quad(
        [
            wall_point(side, geometry, u0, v1, 0.0),
            wall_point(side, geometry, u0, v1, -reveal),
            wall_point(side, geometry, u1, v1, -reveal),
            wall_point(side, geometry, u1, v1, 0.0),
        ],
        geometry.normal,
    );
    // Left jamb.
    wall.push_quad(
        [
            wall_point(side, geometry, u0, v0, 0.0),
            wall_point(side, geometry, u0, v0, -reveal),
            wall_point(side, geometry, u0, v1, -reveal),
            wall_point(side, geometry, u0, v1, 0.0),
        ],
        geometry.normal,
    );
    // Right jamb.
    wall.push_quad(
        [
            wall_point(side, geometry, u1, v0, 0.0),
            wall_point(side, geometry, u1, v1, 0.0),
            wall_point(side, geometry, u1, v1, -reveal),
            wall_point(side, geometry, u1, v0, -reveal),
        ],
        geometry.normal,
    );
}

/// Tessellate one wall band [`y0`, `y1`] into a grid of `bays` x `rows`
/// punched windows: solid wall around each opening, recessed pane in the
/// hole. This is the grammar rung: it turns the spec's bay/floor counts into
/// recognizable facade structure instead of a blank slab.
#[allow(clippy::too_many_arguments)]
fn build_punched_band(
    wall: &mut MassingPart,
    glazing: &mut MassingPart,
    side: Side,
    geometry: &SideGeometry,
    y0: f32,
    y1: f32,
    bays: u32,
    rows: u32,
) {
    let bays = bays.max(1);
    let rows = rows.max(1);
    let total_w = geometry.width;
    let x0 = -total_w / 2.0;
    let cell_w = total_w / bays as f32;
    let cell_h = (y1 - y0) / rows as f32;
    if cell_w <= 0.4 || cell_h <= 0.4 {
        push_wall_band(wall, side, geometry, y0, y1);
        return;
    }
    // Window inside each cell: a centered pane leaving a pier and sill/head.
    let win_w = (cell_w * 0.55).clamp(0.6, cell_w - 0.5);
    let win_h = (cell_h * 0.6).clamp(0.8, cell_h - 0.7);
    let reveal = 0.22_f32.min(geometry.offset * 0.4);
    for row in 0..rows {
        let cy0 = y0 + cell_h * row as f32;
        let cy1 = cy0 + cell_h;
        let wy0 = (cy0 + cy1) / 2.0 - win_h / 2.0;
        let wy1 = (cy0 + cy1) / 2.0 + win_h / 2.0;
        for bay in 0..bays {
            let cx0 = x0 + cell_w * bay as f32;
            let cx1 = cx0 + cell_w;
            let wx0 = (cx0 + cx1) / 2.0 - win_w / 2.0;
            let wx1 = (cx0 + cx1) / 2.0 + win_w / 2.0;
            // Wall panels around the punched window.
            push_wall_rect(wall, side, geometry, cx0, cx1, cy0, wy0); // below
            push_wall_rect(wall, side, geometry, cx0, cx1, wy1, cy1); // above
            push_wall_rect(wall, side, geometry, cx0, wx0, wy0, wy1); // left pier
            push_wall_rect(wall, side, geometry, wx1, cx1, wy0, wy1); // right pier
            push_recessed_opening(wall, glazing, side, geometry, wx0, wx1, wy0, wy1, reveal);
        }
    }
}

/// Emit an axis-aligned box (6 quads) centered at `center` with `size`. Used
/// for the cornice band and other solid trims.
fn push_box(part: &mut MassingPart, center: [f32; 3], size: [f32; 3]) {
    let [cx, cy, cz] = center;
    let (hx, hy, hz) = (size[0] / 2.0, size[1] / 2.0, size[2] / 2.0);
    part.push_quad(
        [
            [cx - hx, cy - hy, cz + hz],
            [cx + hx, cy - hy, cz + hz],
            [cx + hx, cy + hy, cz + hz],
            [cx - hx, cy + hy, cz + hz],
        ],
        [0.0, 0.0, 1.0],
    );
    part.push_quad(
        [
            [cx + hx, cy - hy, cz - hz],
            [cx - hx, cy - hy, cz - hz],
            [cx - hx, cy + hy, cz - hz],
            [cx + hx, cy + hy, cz - hz],
        ],
        [0.0, 0.0, -1.0],
    );
    part.push_quad(
        [
            [cx + hx, cy - hy, cz + hz],
            [cx + hx, cy - hy, cz - hz],
            [cx + hx, cy + hy, cz - hz],
            [cx + hx, cy + hy, cz + hz],
        ],
        [1.0, 0.0, 0.0],
    );
    part.push_quad(
        [
            [cx - hx, cy - hy, cz - hz],
            [cx - hx, cy - hy, cz + hz],
            [cx - hx, cy + hy, cz + hz],
            [cx - hx, cy + hy, cz - hz],
        ],
        [-1.0, 0.0, 0.0],
    );
    part.push_quad(
        [
            [cx - hx, cy + hy, cz + hz],
            [cx + hx, cy + hy, cz + hz],
            [cx + hx, cy + hy, cz - hz],
            [cx - hx, cy + hy, cz - hz],
        ],
        [0.0, 1.0, 0.0],
    );
    part.push_quad(
        [
            [cx - hx, cy - hy, cz - hz],
            [cx + hx, cy - hy, cz - hz],
            [cx + hx, cy - hy, cz + hz],
            [cx - hx, cy - hy, cz + hz],
        ],
        [0.0, -1.0, 0.0],
    );
}

/// True for masonry materials that carry a projecting cornice in this
/// period; wood-frame buildings get an eave from the roof instead.
fn is_masonry(material: &str) -> bool {
    let key = material.to_ascii_lowercase();
    key.contains("brick")
        || key.contains("stone")
        || key.contains("concrete")
        || key.contains("fire")
        || key.contains("masonry")
}

/// Effective window grid for one facade side. Uses the documented opening
/// grid when the spec carries one; otherwise synthesizes a grammar default
/// from the plan span and story count. A synthesized grid is inference: its
/// windows render ghosted even when the wall material is documented, because
/// the window arrangement is a guess, not a record.
struct FacadeGrid {
    bays: u32,
    rows: u32,
    has_storefront: bool,
    flag: PartFlag,
    field_path: String,
}

fn effective_grid(
    side: Side,
    facade: Option<(usize, &Facade)>,
    span_m: f32,
    upper_rows: u32,
    has_ground_floor: bool,
) -> FacadeGrid {
    if let Some((index, facade_ref)) = facade {
        if let Some(grid) = facade_ref.opening_grids.first() {
            return FacadeGrid {
                bays: grid.bay_count.max(1),
                rows: grid.floor_count.max(1).min(upper_rows.max(1)),
                has_storefront: grid.has_storefront_ground,
                flag: classify_part(grid.provenance.as_ref()),
                field_path: format!("facades[{index}].openingGrids[0]"),
            };
        }
    }
    // Grammar default: ~3.4 m bay rhythm, one window row per upper story.
    let min_bays = if matches!(side, Side::Front) { 2 } else { 1 };
    let bays = ((span_m / 3.4).round() as i32).clamp(min_bays, 7) as u32;
    let field_path = match facade {
        Some((index, _)) => format!("facades[{index}].openingGrids[grammar]"),
        None => format!("facades[{}].openingGrids[grammar]", side.as_str()),
    };
    let mut flag = PartFlag::undocumented();
    // A grammar prior is a low-confidence inference, not zero signal.
    flag.confidence = 0.3;
    FacadeGrid {
        bays,
        rows: upper_rows.max(1),
        has_storefront: has_ground_floor && matches!(side, Side::Front),
        flag,
        field_path,
    }
}

/// Normalize a spec facade_side string onto a plan side.
fn match_side(facade_side: &str) -> Option<Side> {
    let normalized = facade_side.to_ascii_lowercase();
    if normalized.contains("front")
        || normalized.contains("primary")
        || normalized.contains("street")
    {
        Some(Side::Front)
    } else if normalized.contains("rear") || normalized.contains("back") {
        Some(Side::Back)
    } else if normalized.contains("right") || normalized.contains("east") {
        Some(Side::Right)
    } else if normalized.contains("left") || normalized.contains("west") {
        Some(Side::Left)
    } else if normalized.contains("south") {
        Some(Side::Front)
    } else if normalized.contains("north") {
        Some(Side::Back)
    } else {
        None
    }
}

/// Assign spec facades to plan sides: explicit side names first, then
/// remaining facades fill remaining sides in order front, right, back, left.
fn assign_facades(spec: &ReconstructionSpec) -> Vec<(Side, Option<(usize, &Facade)>)> {
    let mut assignment: Vec<(Side, Option<(usize, &Facade)>)> =
        Side::ALL.iter().map(|side| (*side, None)).collect();
    let mut unmatched: Vec<(usize, &Facade)> = Vec::new();

    for (index, facade) in spec.facades.iter().enumerate() {
        let matched = match_side(&facade.facade_side).and_then(|side| {
            let slot = assignment
                .iter_mut()
                .find(|(s, occupant)| *s == side && occupant.is_none());
            match slot {
                Some((_, occupant)) => {
                    *occupant = Some((index, facade));
                    Some(())
                }
                None => None,
            }
        });
        if matched.is_none() {
            unmatched.push((index, facade));
        }
    }
    for (index, facade) in unmatched {
        if let Some((_, occupant)) = assignment
            .iter_mut()
            .find(|(_, occupant)| occupant.is_none())
        {
            *occupant = Some((index, facade));
        }
    }
    assignment
}

/// Realize the merged spec as massing geometry. Every produced part carries
/// the provenance flag of the spec part it realizes; sides with no facade
/// entry at all are undocumented walls and render flagged.
pub fn build_massing(spec: &ReconstructionSpec) -> Result<MassingModel> {
    let dims = dims_from_spec(spec);
    anyhow::ensure!(
        dims.width > 0.0 && dims.depth > 0.0 && dims.height > 0.0,
        "degenerate massing dimensions"
    );
    let roof_form = normalize_roof_form(
        spec.roof
            .as_ref()
            .map(|roof| roof.roof_type.as_str())
            .unwrap_or(""),
    );

    let rise = match roof_form {
        RoofForm::Flat => 0.0,
        RoofForm::Gable | RoofForm::Hipped => {
            (dims.height * 0.25).min(dims.width.min(dims.depth) * 0.6)
        }
    };
    let eave = dims.height - rise;
    let ground_flag = classify_part(
        spec.ground_floor
            .as_ref()
            .and_then(|ground| ground.provenance.as_ref()),
    );
    let ground_band = if spec.ground_floor.is_some() {
        (dims.height * 0.3).min(3.6).min(eave * 0.6)
    } else {
        0.0
    };

    let mass_flag = classify_part(spec.mass.as_ref().and_then(|mass| mass.provenance.as_ref()));
    let roof_flag = classify_part(spec.roof.as_ref().and_then(|roof| roof.provenance.as_ref()));

    let mut parts: Vec<MassingPart> = Vec::new();

    // Foundation cap: closes the silhouette from below; carries the mass
    // provenance since it is pure footprint realization.
    let mut foundation = MassingPart::new(
        "mass",
        "Massing footprint",
        PartKind::Foundation,
        mass_flag.clone(),
        "foundation",
        [0.36, 0.34, 0.32, 1.0],
    );
    let half_w = dims.width / 2.0;
    let half_d = dims.depth / 2.0;
    foundation.push_quad(
        [
            [-half_w, 0.0, -half_d],
            [half_w, 0.0, -half_d],
            [half_w, 0.0, half_d],
            [-half_w, 0.0, half_d],
        ],
        [0.0, -1.0, 0.0],
    );
    parts.push(foundation);

    // Walls, ground band, and openings per side.
    let assignments = assign_facades(spec);
    let documented_default_material = spec
        .facades
        .iter()
        .map(|facade| facade.primary_material.as_str())
        .find(|material| !material.is_empty())
        .unwrap_or("");

    let stories = dims.stories.max(1);
    for (side, facade) in &assignments {
        let geometry = side_geometry(*side, &dims);
        let (facade_index, facade_ref) = match facade {
            Some((index, facade)) => (Some(*index), Some(*facade)),
            None => (None, None),
        };
        let flag = match facade_ref {
            Some(facade) => classify_part(facade.provenance.as_ref()),
            None => PartFlag::undocumented(),
        };
        let material = facade_ref
            .map(|facade| facade.primary_material.as_str())
            .filter(|material| !material.is_empty())
            .unwrap_or(documented_default_material);
        let (material_name, color) = if flag.documented {
            wall_color(material)
        } else {
            ("ghost-wall", crate::provenance::ghost_palette::SHADOW)
        };
        let field_path = facade_index
            .map(|index| format!("facades[{index}]"))
            .unwrap_or_else(|| format!("facades[{}]", side.as_str()));
        let mut wall = MassingPart::new(
            field_path.clone(),
            format!("{} facade", side.as_str()),
            PartKind::Wall,
            flag.clone(),
            material_name,
            color,
        );

        // Window grid: documented or grammar-synthesized. Upper rows are the
        // stories above the ground band (the ground floor gets a storefront
        // or door instead of a window row).
        let upper_rows = if ground_band > 0.0 {
            stories.saturating_sub(1).max(1)
        } else {
            stories
        };
        let grid = effective_grid(
            *side,
            *facade,
            geometry.width,
            upper_rows,
            ground_band > 0.0,
        );
        let (glazing_material, glazing_color) = if grid.flag.documented {
            ("glazing", GLASS_COLOR)
        } else {
            ("ghost-glazing", crate::provenance::ghost_palette::HIGHLIGHT)
        };
        let mut glazing = MassingPart::new(
            grid.field_path.clone(),
            format!("{} opening grid", side.as_str()),
            PartKind::Opening,
            grid.flag.clone(),
            glazing_material,
            glazing_color,
        );

        // Punched, recessed window grid over the upper wall band. This is the
        // grammar rung: a building, not a slab, from the spec's bay/floor
        // counts alone.
        build_punched_band(
            &mut wall,
            &mut glazing,
            *side,
            &geometry,
            ground_band,
            eave,
            grid.bays,
            grid.rows,
        );

        // Gable ends: vertical triangles closing the wall up to the ridge on
        // the two sides perpendicular to the ridge.
        if roof_form == RoofForm::Gable && rise > 0.0 {
            let ridge_along_z = dims.depth >= dims.width;
            let is_gable_end = if ridge_along_z {
                matches!(side, Side::Front | Side::Back)
            } else {
                matches!(side, Side::Right | Side::Left)
            };
            if is_gable_end {
                let half = geometry.width / 2.0;
                wall.push_triangle(
                    [
                        wall_point(*side, &geometry, -half, eave, 0.0),
                        wall_point(*side, &geometry, half, eave, 0.0),
                        wall_point(*side, &geometry, 0.0, dims.height, 0.0),
                    ],
                    geometry.normal,
                );
            }
        }

        // Ground-floor band as its own part when the spec carries one.
        if ground_band > 0.0 {
            let (ground_material_name, ground_color) = if ground_flag.documented {
                wall_color(material)
            } else {
                ("ghost-wall", crate::provenance::ghost_palette::SHADOW)
            };
            let mut ground = MassingPart::new(
                "ground_floor",
                format!("{} ground floor", side.as_str()),
                PartKind::GroundFloor,
                ground_flag.clone(),
                ground_material_name,
                ground_color,
            );

            if grid.has_storefront && *side == Side::Front {
                // Storefront: piers + lintel framing a wide recessed window.
                let band_w = (geometry.width * 0.82).min(geometry.width - 1.2);
                let sill = (ground_band * 0.12).max(0.3);
                let head = (ground_band * 0.9).min(ground_band - 0.2);
                let pier = (geometry.width - band_w) / 2.0;
                let half = geometry.width / 2.0;
                push_wall_rect(
                    &mut ground,
                    *side,
                    &geometry,
                    -half,
                    -half + pier,
                    0.0,
                    ground_band,
                );
                push_wall_rect(
                    &mut ground,
                    *side,
                    &geometry,
                    half - pier,
                    half,
                    0.0,
                    ground_band,
                );
                push_wall_rect(
                    &mut ground,
                    *side,
                    &geometry,
                    -half + pier,
                    half - pier,
                    0.0,
                    sill,
                );
                push_wall_rect(
                    &mut ground,
                    *side,
                    &geometry,
                    -half + pier,
                    half - pier,
                    head,
                    ground_band,
                );
                push_recessed_opening(
                    &mut ground,
                    &mut glazing,
                    *side,
                    &geometry,
                    -half + pier,
                    half - pier,
                    sill,
                    head,
                    0.3,
                );
            } else if *side == Side::Front {
                // Solid ground floor with a centered recessed entry door.
                let door_h = (ground_band * 0.85).min(2.4);
                let door_w = 1.2_f32.min(geometry.width * 0.18);
                let half = geometry.width / 2.0;
                push_wall_rect(
                    &mut ground,
                    *side,
                    &geometry,
                    -half,
                    -door_w / 2.0,
                    0.0,
                    ground_band,
                );
                push_wall_rect(
                    &mut ground,
                    *side,
                    &geometry,
                    door_w / 2.0,
                    half,
                    0.0,
                    ground_band,
                );
                push_wall_rect(
                    &mut ground,
                    *side,
                    &geometry,
                    -door_w / 2.0,
                    door_w / 2.0,
                    door_h,
                    ground_band,
                );
                let mut door = MassingPart::new(
                    "ground_floor.entry",
                    "entry door",
                    PartKind::Opening,
                    ground_flag.clone(),
                    "entry-door",
                    [0.20, 0.16, 0.13, 1.0],
                );
                push_recessed_opening(
                    &mut ground,
                    &mut door,
                    *side,
                    &geometry,
                    -door_w / 2.0,
                    door_w / 2.0,
                    0.0,
                    door_h,
                    0.18,
                );
                parts.push(door);
            } else {
                // Side/back ground floor: a punched window row, same grammar.
                build_punched_band(
                    &mut ground,
                    &mut glazing,
                    *side,
                    &geometry,
                    0.0,
                    ground_band,
                    grid.bays,
                    1,
                );
            }
            parts.push(ground);
        }

        parts.push(wall);
        if !glazing.is_empty() {
            parts.push(glazing);
        }
    }

    // Cornice: a projecting trim band at the eave on masonry buildings. The
    // window arrangement and the cornice are both grammar inferences unless a
    // documented ornament says otherwise, so an undocumented cornice renders
    // ghosted.
    let primary_material = spec
        .facades
        .iter()
        .map(|facade| facade.primary_material.as_str())
        .find(|material| !material.is_empty())
        .unwrap_or(documented_default_material);
    if is_masonry(primary_material) && eave > 1.5 {
        let documented_cornice = spec.ornaments.iter().find(|ornament| {
            ornament
                .ornament_kind
                .to_ascii_lowercase()
                .contains("cornice")
                || ornament.location.to_ascii_lowercase().contains("roof")
        });
        let cornice_flag = match documented_cornice {
            Some(ornament) => classify_part(ornament.provenance.as_ref()),
            None => {
                let mut flag = PartFlag::undocumented();
                flag.confidence = 0.3;
                flag
            }
        };
        let (cornice_name, cornice_color) = if cornice_flag.documented {
            ("cornice-stone", [0.66, 0.63, 0.57, 1.0])
        } else {
            ("ghost-cornice", crate::provenance::ghost_palette::MID)
        };
        let mut cornice = MassingPart::new(
            documented_cornice
                .map(|_| "ornaments[cornice]".to_string())
                .unwrap_or_else(|| "ornaments[cornice].grammar".to_string()),
            "cornice".to_string(),
            PartKind::Trim,
            cornice_flag,
            cornice_name,
            cornice_color,
        );
        let projection = 0.35_f32;
        let band_h = (eave * 0.05).clamp(0.35, 0.7);
        push_box(
            &mut cornice,
            [0.0, eave - band_h / 2.0, 0.0],
            [
                dims.width + projection * 2.0,
                band_h,
                dims.depth + projection * 2.0,
            ],
        );
        parts.push(cornice);
    }

    // Roof.
    let roof_material = spec
        .roof
        .as_ref()
        .map(|roof| roof.roof_material.as_str())
        .unwrap_or("");
    let (roof_material_name, roof_base_color) = if roof_flag.documented {
        roof_color(roof_material)
    } else {
        ("ghost-roof", crate::provenance::ghost_palette::MID)
    };
    let mut roof = MassingPart::new(
        "roof",
        format!("{} roof", roof_form.as_str()),
        PartKind::Roof,
        roof_flag,
        roof_material_name,
        roof_base_color,
    );
    build_roof(&mut roof, &dims, roof_form, eave, rise)
        .context("roof geometry construction failed")?;
    parts.push(roof);

    Ok(MassingModel {
        parts: parts.into_iter().filter(|part| !part.is_empty()).collect(),
        dims,
        roof_form,
    })
}

fn normalize(v: [f32; 3]) -> [f32; 3] {
    let length = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    if length <= f32::EPSILON {
        return [0.0, 1.0, 0.0];
    }
    [v[0] / length, v[1] / length, v[2] / length]
}

fn build_roof(
    roof: &mut MassingPart,
    dims: &Dims,
    form: RoofForm,
    eave: f32,
    rise: f32,
) -> Result<()> {
    let half_w = dims.width / 2.0;
    let half_d = dims.depth / 2.0;
    match form {
        RoofForm::Flat => {
            roof.push_quad(
                [
                    [-half_w, eave, half_d],
                    [half_w, eave, half_d],
                    [half_w, eave, -half_d],
                    [-half_w, eave, -half_d],
                ],
                [0.0, 1.0, 0.0],
            );
        }
        RoofForm::Gable => {
            let ridge_y = eave + rise;
            if dims.depth >= dims.width {
                // Ridge along Z at x=0.
                let east_normal = normalize([rise, half_w, 0.0]);
                roof.push_quad(
                    [
                        [half_w, eave, half_d],
                        [half_w, eave, -half_d],
                        [0.0, ridge_y, -half_d],
                        [0.0, ridge_y, half_d],
                    ],
                    east_normal,
                );
                let west_normal = normalize([-rise, half_w, 0.0]);
                roof.push_quad(
                    [
                        [-half_w, eave, -half_d],
                        [-half_w, eave, half_d],
                        [0.0, ridge_y, half_d],
                        [0.0, ridge_y, -half_d],
                    ],
                    west_normal,
                );
            } else {
                // Ridge along X at z=0.
                let south_normal = normalize([0.0, half_d, rise]);
                roof.push_quad(
                    [
                        [-half_w, eave, half_d],
                        [half_w, eave, half_d],
                        [half_w, ridge_y, 0.0],
                        [-half_w, ridge_y, 0.0],
                    ],
                    south_normal,
                );
                let north_normal = normalize([0.0, half_d, -rise]);
                roof.push_quad(
                    [
                        [half_w, eave, -half_d],
                        [-half_w, eave, -half_d],
                        [-half_w, ridge_y, 0.0],
                        [half_w, ridge_y, 0.0],
                    ],
                    north_normal,
                );
            }
        }
        RoofForm::Hipped => {
            let ridge_y = eave + rise;
            // Ridge runs along the long axis, inset by the short half-extent
            // from each end (classic hip), degenerating to a pyramid for a
            // square plan.
            if dims.depth >= dims.width {
                let inset = half_w.min(half_d);
                let ridge_half = (half_d - inset).max(0.0);
                let ridge_a = [0.0, ridge_y, ridge_half];
                let ridge_b = [0.0, ridge_y, -ridge_half];
                // Long slopes (east/west).
                roof.push_quad(
                    [
                        [half_w, eave, half_d],
                        [half_w, eave, -half_d],
                        ridge_b,
                        ridge_a,
                    ],
                    normalize([rise, half_w, 0.0]),
                );
                roof.push_quad(
                    [
                        [-half_w, eave, -half_d],
                        [-half_w, eave, half_d],
                        ridge_a,
                        ridge_b,
                    ],
                    normalize([-rise, half_w, 0.0]),
                );
                // Hip ends (front/back triangles).
                roof.push_triangle(
                    [[-half_w, eave, half_d], [half_w, eave, half_d], ridge_a],
                    normalize([0.0, inset, rise]),
                );
                roof.push_triangle(
                    [[half_w, eave, -half_d], [-half_w, eave, -half_d], ridge_b],
                    normalize([0.0, inset, -rise]),
                );
            } else {
                let inset = half_w.min(half_d);
                let ridge_half = (half_w - inset).max(0.0);
                let ridge_a = [ridge_half, ridge_y, 0.0];
                let ridge_b = [-ridge_half, ridge_y, 0.0];
                roof.push_quad(
                    [
                        [-half_w, eave, half_d],
                        [half_w, eave, half_d],
                        ridge_a,
                        ridge_b,
                    ],
                    normalize([0.0, half_d, rise]),
                );
                roof.push_quad(
                    [
                        [half_w, eave, -half_d],
                        [-half_w, eave, -half_d],
                        ridge_b,
                        ridge_a,
                    ],
                    normalize([0.0, half_d, -rise]),
                );
                roof.push_triangle(
                    [[half_w, eave, half_d], [half_w, eave, -half_d], ridge_a],
                    normalize([rise, inset, 0.0]),
                );
                roof.push_triangle(
                    [[-half_w, eave, -half_d], [-half_w, eave, half_d], ridge_b],
                    normalize([-rise, inset, 0.0]),
                );
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use civic_atlas_types::civic_atlas::v1::{
        Mass, OpeningGrid, PartProvenance, ReconstructionSource, ReconstructionSourceType, Roof,
    };

    fn evidence_provenance() -> PartProvenance {
        PartProvenance {
            sources: vec![ReconstructionSource {
                source_id: "sanborn-1".to_string(),
                source_type: ReconstructionSourceType::Map as i32,
                ..Default::default()
            }],
            part_confidence: 0.9,
            ..Default::default()
        }
    }

    fn two_story_spec(roof_type: &str) -> ReconstructionSpec {
        ReconstructionSpec {
            spec_id: "recon-test-1900".to_string(),
            spec_version: 1,
            mass: Some(Mass {
                provenance: Some(evidence_provenance()),
                stories: 2,
                form: "rectangular".to_string(),
                height: Some(DimensionRange {
                    min: Some(6.5),
                    max: Some(7.5),
                    unit: "m".to_string(),
                }),
                width: Some(DimensionRange {
                    min: Some(10.0),
                    max: Some(10.0),
                    unit: "m".to_string(),
                }),
                depth: Some(DimensionRange {
                    min: Some(14.0),
                    max: Some(14.0),
                    unit: "m".to_string(),
                }),
                ..Default::default()
            }),
            facades: vec![Facade {
                provenance: Some(evidence_provenance()),
                facade_side: "front".to_string(),
                primary_material: "brick".to_string(),
                opening_grids: vec![OpeningGrid {
                    provenance: Some(evidence_provenance()),
                    bay_count: 3,
                    floor_count: 2,
                    window_pattern: "regular".to_string(),
                    ..Default::default()
                }],
                ..Default::default()
            }],
            roof: Some(Roof {
                provenance: Some(evidence_provenance()),
                roof_type: roof_type.to_string(),
                roof_material: "slate".to_string(),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    #[test]
    fn dims_resolve_from_ranges_with_unit_conversion() {
        let mut spec = two_story_spec("flat");
        spec.mass.as_mut().unwrap().width = Some(DimensionRange {
            min: Some(30.0),
            max: Some(34.0),
            unit: "ft".to_string(),
        });
        let dims = dims_from_spec(&spec);
        assert!(
            (dims.width - 9.7536).abs() < 1e-3,
            "feet should convert to meters, got {}",
            dims.width
        );
        assert_eq!(dims.stories, 2);
    }

    #[test]
    fn flat_roof_massing_has_walls_roof_and_openings() {
        let model = build_massing(&two_story_spec("flat")).unwrap();
        assert_eq!(model.roof_form, RoofForm::Flat);
        let kinds: Vec<_> = model.parts.iter().map(|part| part.kind).collect();
        assert!(kinds.contains(&PartKind::Wall));
        assert!(kinds.contains(&PartKind::Roof));
        assert!(kinds.contains(&PartKind::Opening));
        let openings = model
            .parts
            .iter()
            .find(|part| part.field_path == "facades[0].openingGrids[0]")
            .expect("front opening grid part");
        // 3 bays x 2 floors = 6 windows, 4 vertices each.
        assert_eq!(openings.positions.len(), 24);
    }

    #[test]
    fn gable_roof_rises_to_full_height() {
        let model = build_massing(&two_story_spec("gable")).unwrap();
        assert_eq!(model.roof_form, RoofForm::Gable);
        let max_y = model
            .parts
            .iter()
            .flat_map(|part| part.positions.iter().map(|p| p[1]))
            .fold(f32::MIN, f32::max);
        assert!((max_y - model.dims.height).abs() < 1e-4);
    }

    #[test]
    fn hipped_roof_is_built() {
        let model = build_massing(&two_story_spec("hipped")).unwrap();
        assert_eq!(model.roof_form, RoofForm::Hipped);
        let roof = model
            .parts
            .iter()
            .find(|part| part.field_path == "roof")
            .unwrap();
        assert_eq!(roof.kind, PartKind::Roof);
        // 2 quads (8 vertices) + 2 triangles (6 vertices).
        assert_eq!(roof.positions.len(), 14);
    }

    #[test]
    fn grammar_synthesizes_windows_when_spec_has_no_grid() {
        // A facade documented as brick but carrying no opening grid: the
        // wall reads documented, the windows are a grammar inference.
        let mut spec = two_story_spec("flat");
        spec.facades[0].opening_grids.clear();
        let model = build_massing(&spec).unwrap();
        let front_glazing = model
            .parts
            .iter()
            .find(|part| {
                part.field_path
                    .starts_with("facades[0].openingGrids[grammar]")
            })
            .expect("front facade gets a synthesized grammar grid");
        assert_eq!(front_glazing.kind, PartKind::Opening);
        assert!(
            !front_glazing.flag.documented,
            "synthesized windows are inference"
        );
        assert!(
            !front_glazing.positions.is_empty(),
            "windows are actually built"
        );
        let front_wall = model
            .parts
            .iter()
            .find(|part| part.field_path == "facades[0]" && part.kind == PartKind::Wall)
            .unwrap();
        assert!(
            front_wall.flag.documented,
            "documented brick wall stays documented"
        );
    }

    #[test]
    fn masonry_gets_a_cornice() {
        let model = build_massing(&two_story_spec("flat")).unwrap();
        let cornice = model
            .parts
            .iter()
            .find(|part| part.kind == PartKind::Trim)
            .expect("brick building gets a cornice trim");
        assert_eq!(cornice.positions.len(), 24, "cornice is a 6-face box");
    }

    #[test]
    fn undocumented_sides_render_ghosted() {
        let model = build_massing(&two_story_spec("flat")).unwrap();
        let documented_walls: Vec<_> = model
            .parts
            .iter()
            .filter(|part| part.kind == PartKind::Wall && part.flag.documented)
            .collect();
        let ghost_walls: Vec<_> = model
            .parts
            .iter()
            .filter(|part| part.kind == PartKind::Wall && !part.flag.documented)
            .collect();
        // One documented front facade; the other three sides have no facade
        // entry and must render flagged.
        assert_eq!(documented_walls.len(), 1);
        assert_eq!(ghost_walls.len(), 3);
        for wall in ghost_walls {
            assert_eq!(wall.material_name, "ghost-wall");
            assert_eq!(wall.base_color, crate::provenance::ghost_palette::SHADOW);
        }
    }

    #[test]
    fn geometry_is_centered_and_grounded() {
        let model = build_massing(&two_story_spec("flat")).unwrap();
        let mut min = [f32::MAX; 3];
        let mut max = [f32::MIN; 3];
        for part in &model.parts {
            for p in &part.positions {
                for axis in 0..3 {
                    min[axis] = min[axis].min(p[axis]);
                    max[axis] = max[axis].max(p[axis]);
                }
            }
        }
        assert!((min[1] - 0.0).abs() < 1e-4, "ground plane at y=0");
        assert!((min[0] + max[0]).abs() < 0.2, "centered on x");
        assert!((min[2] + max[2]).abs() < 0.2, "centered on z");
        // Width span is the wall envelope plus the cornice projection
        // (0.35 m each side on masonry); never less than the footprint.
        let x_span = max[0] - min[0];
        assert!(
            x_span >= model.dims.width - 0.01 && x_span <= model.dims.width + 0.8,
            "x span {x_span} within footprint + cornice projection of width {}",
            model.dims.width
        );
    }
}

"""Tests for the assembly placement bridge: run_cadquery_build capturing
cq.Assembly placements to assembly/placements.json, and glb_export applying
them to component meshes."""
import json
import sys
from pathlib import Path

import numpy as np
import pytest

cadquery = pytest.importorskip("cadquery")

REPO_ROOT = Path(__file__).resolve().parents[2]
MCP_DIR = REPO_ROOT / "mcp"
SRC_DIR = REPO_ROOT / "python" / "src"
if str(MCP_DIR) not in sys.path:
    sys.path.insert(0, str(MCP_DIR))
if str(SRC_DIR) not in sys.path:
    sys.path.insert(0, str(SRC_DIR))

import server  # noqa: E402
from ai3d_cad import glb_export  # noqa: E402


ASSEMBLY_CODE = '''
import cadquery as cq

a = cq.Workplane("XY").box(10, 10, 10)
b = cq.Workplane("XY").box(10, 10, 10)

assy = cq.Assembly()
assy.add(a, name="base")
assy.add(b, loc=cq.Location((0, 0, 25)), name="lid")

result = assy.toCompound()
'''


def test_run_cadquery_build_writes_placements_json(tmp_path):
    session_dir = tmp_path / "session"
    session_dir.mkdir()
    output_dir = session_dir / "assembly"

    result = server.run_cadquery_build(
        ASSEMBLY_CODE, str(output_dir), session_root=str(session_dir), label="assembly"
    )

    assert result["success"] is True

    placements_path = session_dir / "assembly" / "placements.json"
    assert placements_path.exists()

    data = json.loads(placements_path.read_text())
    names = {p["name"]: p for p in data["placements"]}
    assert "lid" in names
    matrix = names["lid"]["matrix"]
    assert len(matrix) == 16
    # row-major 4x4, translation z is row 2, col 3 -> index 2*4+3 = 11
    assert matrix[11] == pytest.approx(25.0, abs=1e-6)


def test_placements_applied_to_component_meshes(tmp_path):
    session_dir = tmp_path / "session"
    session_dir.mkdir()
    output_dir = session_dir / "assembly"

    server.run_cadquery_build(
        ASSEMBLY_CODE, str(output_dir), session_root=str(session_dir), label="assembly"
    )

    import trimesh
    comps = {
        "base": trimesh.creation.box(extents=(10, 10, 10)),
        "lid": trimesh.creation.box(extents=(10, 10, 10)),
    }

    placements = glb_export.load_placements(session_dir)
    placed = glb_export.apply_placements(comps, placements)

    assert not np.allclose(placed["lid"].vertices, comps["lid"].vertices)
    assert np.allclose(placed["lid"].vertices[:, 2], comps["lid"].vertices[:, 2] + 25.0)
    assert np.allclose(placed["base"].vertices, comps["base"].vertices)


ROTATED_ASSEMBLY_CODE = '''
import cadquery as cq

a = cq.Workplane("XY").box(10, 10, 10)
b = cq.Workplane("XY").box(10, 10, 10)

assy = cq.Assembly()
assy.add(a, name="base")
assy.add(b, loc=cq.Location(cq.Vector(5, 0, 30), cq.Vector(0, 0, 1), 90), name="lid")

result = assy.toCompound()
'''


def test_run_cadquery_build_rotation_bearing_placement_matches_compound(tmp_path):
    """A placement with both rotation and translation must reproduce the same
    world-space bounds as the exported assembly compound STL — encodes the
    manually-verified 'placed component bounds match compound STL bounds'
    check so a transposed/column-major regression in the rotation block is
    caught."""
    session_dir = tmp_path / "session"
    session_dir.mkdir()
    output_dir = session_dir / "assembly"

    result = server.run_cadquery_build(
        ROTATED_ASSEMBLY_CODE, str(output_dir), session_root=str(session_dir), label="assembly"
    )
    assert result["success"] is True

    import trimesh
    compound_mesh = trimesh.load_mesh(str(output_dir / "result.stl"), process=False)
    compound_bounds = compound_mesh.bounds

    comps = {
        "base": trimesh.creation.box(extents=(10, 10, 10)),
        "lid": trimesh.creation.box(extents=(10, 10, 10)),
    }
    placements = glb_export.load_placements(session_dir)
    placed = glb_export.apply_placements(comps, placements)

    combined = trimesh.util.concatenate([placed["base"], placed["lid"]])
    assert np.allclose(combined.bounds, compound_bounds, atol=1e-4)

    # Sanity: the rotation actually changed lid's footprint (x/y extents
    # swap relative to an axis-aligned translate-only placement).
    assert not np.allclose(
        placed["lid"].vertices[:, :2], comps["lid"].vertices[:, :2] + np.array([5.0, 0.0])
    )

    # The bounds comparison above cannot discriminate a transposed (inverse)
    # rotation for symmetric parts, so pin the emitted matrix entries
    # directly. A +90° rotation about Z is, row-major:
    #   [ 0 -1  0  5]
    #   [ 1  0  0  0]
    #   [ 0  0  1 30]
    # A transposed 3x3 block flips the signs of rows/cols (0,1) and (1,0).
    lid_matrix = placements["lid"]
    assert lid_matrix[0, 1] == pytest.approx(-1.0, abs=1e-9)
    assert lid_matrix[1, 0] == pytest.approx(1.0, abs=1e-9)
    assert lid_matrix[0, 3] == pytest.approx(5.0, abs=1e-9)
    assert lid_matrix[2, 3] == pytest.approx(30.0, abs=1e-9)


NESTED_ASSEMBLY_CODE = '''
import cadquery as cq

motor = cq.Workplane("XY").box(10, 10, 10)
gearbox = cq.Workplane("XY").box(10, 10, 10)

sub = cq.Assembly()
sub.add(motor, name="motor")
sub.add(gearbox, loc=cq.Location((0, 0, 12)), name="gearbox")

base = cq.Workplane("XY").box(10, 10, 10)

assy = cq.Assembly()
assy.add(base, name="base")
assy.add(sub, loc=cq.Location((0, 0, 25)), name="drivetrain")

result = assy.toCompound()
'''


def test_nested_sub_assembly_emits_leaf_component_names(tmp_path):
    session_dir = tmp_path / "session"
    session_dir.mkdir()
    output_dir = session_dir / "assembly"

    result = server.run_cadquery_build(
        NESTED_ASSEMBLY_CODE, str(output_dir), session_root=str(session_dir), label="assembly"
    )
    assert result["success"] is True

    placements_path = session_dir / "assembly" / "placements.json"
    data = json.loads(placements_path.read_text())
    names = {p["name"]: p for p in data["placements"]}

    # Leaf components, not the "drivetrain" sub-assembly grouping name.
    assert "motor" in names
    assert "gearbox" in names
    assert "drivetrain" not in names

    # gearbox world z = sub-assembly offset (25) + its own local offset (12)
    gearbox_matrix = names["gearbox"]["matrix"]
    assert gearbox_matrix[11] == pytest.approx(37.0, abs=1e-6)
    motor_matrix = names["motor"]["matrix"]
    assert motor_matrix[11] == pytest.approx(25.0, abs=1e-6)


def test_failed_assembly_build_preserves_placements(tmp_path):
    """A transient error in the agent's assembly code must not wipe the
    placements of the last successful build — the viewer still shows that
    iteration, and collapsing it to parts-at-origin would be a regression."""
    session_dir = tmp_path / "session"
    session_dir.mkdir()
    output_dir = session_dir / "assembly"

    result = server.run_cadquery_build(
        ASSEMBLY_CODE, str(output_dir), session_root=str(session_dir), label="assembly"
    )
    assert result["success"] is True
    placements_path = session_dir / "assembly" / "placements.json"
    before = placements_path.read_text()

    result = server.run_cadquery_build(
        "import cadquery as cq\nresult = undefined_name\n",
        str(output_dir), session_root=str(session_dir), label="assembly",
    )
    assert result["success"] is False
    assert placements_path.read_text() == before
    assert not placements_path.with_suffix(".json.prev").exists()


OBJ_BEARING_NODES_CODE = '''
import cadquery as cq

base = cq.Workplane("XY").box(10, 10, 10)
lid = cq.Workplane("XY").box(10, 10, 10)
motor = cq.Workplane("XY").box(10, 10, 10)
gear = cq.Workplane("XY").box(10, 10, 10)

# Root constructed WITH geometry and its own location — mainstream idiom.
assy = cq.Assembly(base, name="chassis", loc=cq.Location((0, 0, 7)))
assy.add(lid, loc=cq.Location((0, 0, 14)), name="lid")

# Mid-tree node carrying BOTH its own obj and children.
sub = cq.Assembly(motor, name="motor")
sub.add(gear, loc=cq.Location((0, 0, 8)), name="gear")
assy.add(sub, loc=cq.Location((0, 0, 25)), name="motor")

result = assy.toCompound()
'''


def test_obj_bearing_root_and_midtree_nodes_get_placements(tmp_path):
    """Nodes that carry their own `obj` must emit placements even when they
    are the root or have children — skipping them left those meshes at
    identity in the viewer (the exact parts-stacked-at-origin bug)."""
    session_dir = tmp_path / "session"
    session_dir.mkdir()
    output_dir = session_dir / "assembly"

    result = server.run_cadquery_build(
        OBJ_BEARING_NODES_CODE, str(output_dir), session_root=str(session_dir), label="assembly"
    )
    assert result["success"] is True

    data = json.loads((session_dir / "assembly" / "placements.json").read_text())
    names = {p["name"]: p for p in data["placements"]}

    assert set(names) == {"chassis", "lid", "motor", "gear"}
    # Root obj at its own loc; children compose root loc × their own.
    assert names["chassis"]["matrix"][11] == pytest.approx(7.0, abs=1e-6)
    assert names["lid"]["matrix"][11] == pytest.approx(21.0, abs=1e-6)
    assert names["motor"]["matrix"][11] == pytest.approx(32.0, abs=1e-6)
    assert names["gear"]["matrix"][11] == pytest.approx(40.0, abs=1e-6)


COMPONENT_ASSEMBLY_CODE = '''
import cadquery as cq

w = cq.Workplane("XY").box(4, 4, 4)

# A component build whose code happens to use cq.Assembly internally.
scratch = cq.Assembly()
scratch.add(w, loc=cq.Location((0, 0, 999)), name="widget")

result = w
'''


def test_non_assembly_build_does_not_clobber_placements(tmp_path):
    """Only an assembly-dir build may write placements.json. A component /
    refinement / imported build whose code happens to construct a cq.Assembly
    must leave the real assembly placements untouched — nothing would ever
    clear a clobbered file, and every component would fall back to identity
    (parts stacked at origin, the exact bug this bridge exists to fix)."""
    session_dir = tmp_path / "session"
    session_dir.mkdir()
    placements_path = session_dir / "assembly" / "placements.json"

    server.run_cadquery_build(
        ASSEMBLY_CODE,
        str(session_dir / "assembly"),
        session_root=str(session_dir),
        label="assembly",
    )
    before = json.loads(placements_path.read_text())
    assert {p["name"] for p in before["placements"]} == {"base", "lid"}

    # Component build, label = the component name.
    comp_result = server.run_cadquery_build(
        COMPONENT_ASSEMBLY_CODE,
        str(session_dir / "components" / "lid"),
        session_root=str(session_dir),
        label="lid",
    )
    assert comp_result["success"] is True

    after = json.loads(placements_path.read_text())
    assert after == before
    assert "widget" not in {p["name"] for p in after["placements"]}

    # A refinement build behaves the same way.
    refine_result = server.run_cadquery_build(
        COMPONENT_ASSEMBLY_CODE,
        str(session_dir / "refinement"),
        session_root=str(session_dir),
        label="refinement",
    )
    assert refine_result["success"] is True
    assert json.loads(placements_path.read_text()) == before


def test_non_assembly_build_returning_an_assembly_still_exports(tmp_path):
    """`result` being a cq.Assembly is unwrapped to a compound whatever the
    label, so STL/STEP export keeps working — only the placements.json write
    is label-gated."""
    session_dir = tmp_path / "session"
    session_dir.mkdir()
    output_dir = session_dir / "components" / "lid"

    result = server.run_cadquery_build(
        ASSEMBLY_CODE.replace("result = assy.toCompound()", "result = assy"),
        str(output_dir),
        session_root=str(session_dir),
        label="lid",
    )

    assert result["success"] is True
    assert (output_dir / "result.stl").exists()
    assert not (session_dir / "assembly" / "placements.json").exists()


def test_export_build_iteration_instances_multiple_nodes_per_component(tmp_path):
    """Two placements for the SAME component ('widget_0'/'widget_1') must
    produce two distinct named nodes in the exported GLB, each traceable to
    its source component in the manifest — the multi-instance case the
    one-mesh-per-component GLB previously could not represent."""
    session_dir = tmp_path / "session"
    widget_dir = session_dir / "components" / "widget"
    widget_dir.mkdir(parents=True)

    import trimesh
    trimesh.creation.box(extents=(10, 10, 10)).export(str(widget_dir / "result.stl"))

    assembly_dir = session_dir / "assembly"
    assembly_dir.mkdir(parents=True)
    matrix0 = [1.0, 0.0, 0.0, 0.0,
               0.0, 1.0, 0.0, 0.0,
               0.0, 0.0, 1.0, 0.0,
               0.0, 0.0, 0.0, 1.0]
    matrix1 = [1.0, 0.0, 0.0, 0.0,
               0.0, 1.0, 0.0, 0.0,
               0.0, 0.0, 1.0, 25.0,
               0.0, 0.0, 0.0, 1.0]
    (assembly_dir / "placements.json").write_text(
        json.dumps({"placements": [
            {"name": "widget_0", "matrix": matrix0},
            {"name": "widget_1", "matrix": matrix1},
        ]})
    )

    n = server._export_build_iteration(str(session_dir), "build", None, None)
    assert n is not None

    glb_path = session_dir / f"iteration_{n:03d}.glb"
    manifest_path = session_dir / f"iteration_{n:03d}.manifest.json"
    assert glb_path.exists()
    assert manifest_path.exists()

    loaded = trimesh.load(str(glb_path), file_type="glb")
    names = set(loaded.geometry.keys()) | set(loaded.graph.nodes_geometry)
    assert "widget_0" in names
    assert "widget_1" in names

    # Compare via geometry transforms baked into the mesh vertices instead
    # of relying on scene-graph node transforms (bake-in export contract).
    geom0 = loaded.geometry["widget_0"] if "widget_0" in loaded.geometry else None
    geom1 = loaded.geometry["widget_1"] if "widget_1" in loaded.geometry else None
    assert geom0 is not None and geom1 is not None
    assert np.allclose(geom1.vertices[:, 2], geom0.vertices[:, 2] + 25.0)

    manifest = json.loads(manifest_path.read_text())
    entries = {c["name"]: c for c in manifest["components"]}
    assert entries["widget_0"]["component"] == "widget"
    assert entries["widget_1"]["component"] == "widget"


def test_stale_placements_json_removed_when_assembly_build_no_longer_produces_one(tmp_path):
    session_dir = tmp_path / "session"
    session_dir.mkdir()
    output_dir = session_dir / "assembly"

    # First build writes placements.json.
    server.run_cadquery_build(
        ASSEMBLY_CODE, str(output_dir), session_root=str(session_dir), label="assembly"
    )
    placements_path = session_dir / "assembly" / "placements.json"
    assert placements_path.exists()

    # Second build in the same assembly dir no longer produces a cq.Assembly
    # (e.g. rewritten as a manually-composed compound). Stale placements from
    # the first build must not silently keep applying.
    plain_code = '''
import cadquery as cq
result = cq.Workplane("XY").box(10, 10, 10)
'''
    result = server.run_cadquery_build(
        plain_code, str(output_dir), session_root=str(session_dir), label="assembly"
    )
    assert result["success"] is True
    assert not placements_path.exists()

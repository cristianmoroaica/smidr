"""Tests for ai3d_cad.glb_export — mesh-level GLB/manifest export (no cadquery)."""
import json
import os

import numpy as np
import trimesh

from ai3d_cad import glb_export


def _two_box_scene():
    base = trimesh.creation.box(extents=(10, 20, 30))
    lid = trimesh.creation.box(extents=(10, 20, 30))
    lid.apply_translation((5, 0, 0))
    return {"base": base, "lid": lid}


def test_export_glb_writes_valid_glb_with_named_nodes(tmp_path):
    components = _two_box_scene()
    out_path = tmp_path / "scene.glb"
    glb_export.export_glb(components, out_path)

    assert out_path.exists()
    data = out_path.read_bytes()
    assert len(data) > 0
    assert data[:4] == b"glTF"

    loaded = trimesh.load(str(out_path), file_type="glb")
    assert isinstance(loaded, trimesh.Scene)
    names = set(loaded.geometry.keys()) | set(loaded.graph.nodes_geometry)
    assert "base" in names
    assert "lid" in names


def test_export_glb_empty_components_raises(tmp_path):
    try:
        glb_export.export_glb({}, tmp_path / "empty.glb")
        assert False, "expected ValueError"
    except ValueError:
        pass


def test_mesh_hash_stable_for_identical_geometry():
    box1 = trimesh.creation.box(extents=(10, 20, 30))
    box2 = trimesh.creation.box(extents=(10, 20, 30))
    assert glb_export.mesh_hash(box1) == glb_export.mesh_hash(box2)


def test_mesh_hash_differs_for_translated_copy():
    box1 = trimesh.creation.box(extents=(10, 20, 30))
    box2 = trimesh.creation.box(extents=(10, 20, 30))
    box2.apply_translation((5, 0, 0))
    assert glb_export.mesh_hash(box1) != glb_export.mesh_hash(box2)


def test_write_manifest_bbox_and_dimensions(tmp_path):
    box = trimesh.creation.box(extents=(10, 20, 30))
    box.apply_translation((5, 0, 0))
    components = {"base": box}
    spec_dims = {"x": 10, "y": 20, "z": 30}
    out_path = tmp_path / "manifest.json"
    glb_export.write_manifest(components, spec_dims, out_path)

    with open(out_path) as f:
        manifest = json.load(f)

    assert manifest["dimensions"] == spec_dims
    assert len(manifest["components"]) == 1
    comp = manifest["components"][0]
    assert comp["name"] == "base"

    bounds = box.bounds
    got_min, got_max = comp["bbox"]
    assert np.allclose(got_min, bounds[0], atol=1e-6)
    assert np.allclose(got_max, bounds[1], atol=1e-6)
    assert comp["mesh_hash"] == glb_export.mesh_hash(box)


def test_write_manifest_preserves_component_order():
    boxes = {}
    for name in ["z_last", "a_first", "m_mid"]:
        b = trimesh.creation.box(extents=(1, 1, 1))
        boxes[name] = b
    import tempfile
    with tempfile.TemporaryDirectory() as d:
        out_path = os.path.join(d, "manifest.json")
        glb_export.write_manifest(boxes, {}, out_path)
        with open(out_path) as f:
            manifest = json.load(f)
    names = [c["name"] for c in manifest["components"]]
    assert names == ["z_last", "a_first", "m_mid"]


def test_next_iteration_and_export_iteration_append_only(tmp_path):
    session_dir = tmp_path / "session"
    components = _two_box_scene()

    assert glb_export.next_iteration(session_dir) == 1

    n1 = glb_export.export_iteration(session_dir, components, {"x": 10, "y": 20, "z": 30})
    assert n1 == 1
    glb1 = session_dir / "iteration_001.glb"
    manifest1 = session_dir / "iteration_001.manifest.json"
    assert glb1.exists()
    assert manifest1.exists()
    glb1_mtime = glb1.stat().st_mtime
    glb1_bytes = glb1.read_bytes()

    n2 = glb_export.export_iteration(session_dir, components, {"x": 10, "y": 20, "z": 30})
    assert n2 == 2
    glb2 = session_dir / "iteration_002.glb"
    manifest2 = session_dir / "iteration_002.manifest.json"
    assert glb2.exists()
    assert manifest2.exists()

    # first pair untouched
    assert glb1.exists()
    assert glb1.stat().st_mtime == glb1_mtime
    assert glb1.read_bytes() == glb1_bytes

    assert glb_export.next_iteration(session_dir) == 3


def test_next_iteration_missing_dir_returns_1(tmp_path):
    missing = tmp_path / "does_not_exist"
    assert glb_export.next_iteration(missing) == 1


def test_load_components_skips_missing_and_bad_paths(tmp_path):
    box = trimesh.creation.box(extents=(1, 1, 1))
    stl_path = tmp_path / "base.stl"
    box.export(str(stl_path))

    paths = {
        "base": str(stl_path),
        "missing": str(tmp_path / "nope.stl"),
    }
    loaded = glb_export.load_components(paths)
    assert "base" in loaded
    assert "missing" not in loaded
    assert isinstance(loaded["base"], trimesh.Trimesh)


# ── load_placements / apply_placements ──

def test_load_placements_missing_file_returns_empty(tmp_path):
    session_dir = tmp_path / "session"
    session_dir.mkdir()
    assert glb_export.load_placements(session_dir) == {}


def test_load_placements_valid_file_returns_lowercased_keys(tmp_path):
    session_dir = tmp_path / "session"
    assembly_dir = session_dir / "assembly"
    assembly_dir.mkdir(parents=True)
    matrix = [1.0, 0.0, 0.0, 0.0,
              0.0, 1.0, 0.0, 0.0,
              0.0, 0.0, 1.0, 25.0,
              0.0, 0.0, 0.0, 1.0]
    payload = {"placements": [{"name": "Lid", "matrix": matrix}]}
    (assembly_dir / "placements.json").write_text(json.dumps(payload))

    placements = glb_export.load_placements(session_dir)
    assert "lid" in placements
    assert isinstance(placements["lid"], np.ndarray)
    assert placements["lid"].shape == (4, 4)
    assert placements["lid"][2, 3] == 25.0


def test_load_placements_malformed_json_returns_empty(tmp_path):
    session_dir = tmp_path / "session"
    assembly_dir = session_dir / "assembly"
    assembly_dir.mkdir(parents=True)
    (assembly_dir / "placements.json").write_text("{not valid json")

    assert glb_export.load_placements(session_dir) == {}


def test_apply_placements_empty_placements_returns_unchanged(tmp_path):
    components = _two_box_scene()
    result = glb_export.apply_placements(components, {})
    # Implemented contract: an empty placements dict is a no-op short-circuit
    # that returns the very same object, not just an equal one.
    assert result is components
    assert set(result.keys()) == {"base", "lid"}


def test_apply_placements_matched_component_transformed_unmatched_kept_identity(capsys):
    components = _two_box_scene()
    translate_z = np.eye(4)
    translate_z[2, 3] = 25.0
    placements = {"lid": translate_z}

    result = glb_export.apply_placements(components, placements)

    assert set(result.keys()) == {"base", "lid"}
    # matched component transformed
    assert not np.allclose(result["lid"].vertices, components["lid"].vertices)
    assert np.allclose(
        result["lid"].vertices[:, 2], components["lid"].vertices[:, 2] + 25.0
    )
    # unmatched component kept identity
    assert np.allclose(result["base"].vertices, components["base"].vertices)
    # original components dict/meshes not mutated
    assert np.allclose(components["lid"].vertices, _two_box_scene()["lid"].vertices)

    # the unmatched component is reported on stderr
    captured = capsys.readouterr()
    assert "no placement for component 'base'; using identity" in captured.err
    # ...and the matched one is not warned about
    assert "no placement for component 'lid'" not in captured.err


def test_apply_placements_does_not_mutate_input_dict():
    components = _two_box_scene()
    original_keys = set(components.keys())
    placements = {"base": np.eye(4)}
    result = glb_export.apply_placements(components, placements)
    assert set(components.keys()) == original_keys
    assert result is not components


def test_apply_placements_rotation_matches_expected_bounds():
    # A 90-degree rotation about Z combined with a translation, expressed as
    # a plain row-major 4x4 (not derived from cadquery), so a
    # transposed/column-major regression in a caller's matrix construction
    # would move vertices to the wrong place and fail this assertion.
    lid = trimesh.creation.box(extents=(10, 20, 30))
    components = {"lid": lid}

    # 90 degree rotation about Z: x' = -y, y' = x, z' = z, plus translation.
    theta = np.pi / 2
    c, s = np.cos(theta), np.sin(theta)
    matrix = np.array([
        [c, -s, 0.0, 5.0],
        [s, c, 0.0, 0.0],
        [0.0, 0.0, 1.0, 30.0],
        [0.0, 0.0, 0.0, 1.0],
    ])
    placements = {"lid": matrix}

    result = glb_export.apply_placements(components, placements)

    expected = lid.copy()
    expected.apply_transform(matrix)
    assert np.allclose(result["lid"].vertices, expected.vertices)

    # Box is centered at origin (z in -15..15); translation by 30 on Z
    # shifts it to z in 15..45. Rotation about Z alone doesn't move z.
    assert np.isclose(result["lid"].bounds[0][2], 15.0, atol=1e-6)
    assert np.isclose(result["lid"].bounds[1][2], 45.0, atol=1e-6)


def test_apply_placements_orphan_placement_warns(capsys):
    components = {"base": trimesh.creation.box(extents=(1, 1, 1))}
    placements = {
        "base": np.eye(4),
        "ghost": np.eye(4),
    }

    glb_export.apply_placements(components, placements)

    captured = capsys.readouterr()
    assert "no component for placement" in captured.err
    assert "ghost" in captured.err


# ── build_scene_nodes ──

def test_build_scene_nodes_two_instances_of_one_component():
    box = trimesh.creation.box(extents=(10, 10, 10))
    components = {"widget": box}
    t0 = np.eye(4)
    t1 = np.eye(4)
    t1[2, 3] = 25.0
    placements = {"widget_0": t0, "widget_1": t1}

    nodes, sources = glb_export.build_scene_nodes(components, placements)

    assert set(nodes.keys()) == {"widget_0", "widget_1"}
    assert sources["widget_0"] == "widget"
    assert sources["widget_1"] == "widget"

    z0 = nodes["widget_0"].vertices[:, 2]
    z1 = nodes["widget_1"].vertices[:, 2]
    assert np.allclose(z1, z0 + 25.0)

    # source component mesh untouched
    assert np.allclose(components["widget"].vertices, box.vertices)


def test_build_scene_nodes_single_instance_exact_match():
    box = trimesh.creation.box(extents=(10, 10, 10))
    components = {"widget": box}
    placements = {"widget": np.eye(4)}

    nodes, sources = glb_export.build_scene_nodes(components, placements)

    assert set(nodes.keys()) == {"widget"}
    assert sources["widget"] == "widget"


def test_build_scene_nodes_suffix_strip_match():
    box = trimesh.creation.box(extents=(10, 10, 10))
    components = {"column_lower": box}
    placements = {"column_lower_0": np.eye(4)}

    nodes, sources = glb_export.build_scene_nodes(components, placements)

    assert set(nodes.keys()) == {"column_lower_0"}
    assert sources["column_lower_0"] == "column_lower"


def test_build_scene_nodes_non_matching_suffix_strip_skipped(capsys):
    box = trimesh.creation.box(extents=(10, 10, 10))
    components = {"column_lower": box}
    placements = {"col_lower_0": np.eye(4)}

    nodes, sources = glb_export.build_scene_nodes(components, placements)

    # The placement node itself is skipped...
    assert "col_lower_0" not in nodes
    captured = capsys.readouterr()
    assert "no component for placement" in captured.err
    assert "col_lower_0" in captured.err

    # ...but the unreferenced component still surfaces once at identity, so
    # nothing silently disappears from the scene.
    assert set(nodes.keys()) == {"column_lower"}
    assert sources["column_lower"] == "column_lower"
    assert np.allclose(nodes["column_lower"].vertices, box.vertices)
    assert "no placement for component 'column_lower'; using identity" in captured.err


def test_build_scene_nodes_component_absent_from_placements_appears_at_identity(capsys):
    base = trimesh.creation.box(extents=(10, 10, 10))
    lid = trimesh.creation.box(extents=(10, 10, 10))
    components = {"base": base, "lid": lid}
    placements = {"lid": np.eye(4)}

    nodes, sources = glb_export.build_scene_nodes(components, placements)

    assert set(nodes.keys()) == {"base", "lid"}
    assert sources["base"] == "base"
    assert np.allclose(nodes["base"].vertices, base.vertices)

    captured = capsys.readouterr()
    assert "no placement for component 'base'; using identity" in captured.err


def test_build_scene_nodes_empty_placements_returns_legacy_identity():
    base = trimesh.creation.box(extents=(10, 10, 10))
    lid = trimesh.creation.box(extents=(10, 10, 10))
    components = {"base": base, "lid": lid}

    nodes, sources = glb_export.build_scene_nodes(components, {})

    # same objects, no copy
    assert nodes["base"] is base
    assert nodes["lid"] is lid
    assert sources == {"base": "base", "lid": "lid"}


def test_write_manifest_with_sources_includes_component_field(tmp_path):
    box = trimesh.creation.box(extents=(10, 10, 10))
    components = {"widget_0": box}
    sources = {"widget_0": "widget"}
    out_path = tmp_path / "manifest.json"
    glb_export.write_manifest(components, {}, out_path, sources=sources)

    with open(out_path) as f:
        manifest = json.load(f)

    comp = manifest["components"][0]
    assert comp["name"] == "widget_0"
    assert comp["component"] == "widget"


def test_write_manifest_without_sources_component_equals_name(tmp_path):
    box = trimesh.creation.box(extents=(10, 10, 10))
    components = {"base": box}
    out_path = tmp_path / "manifest.json"
    glb_export.write_manifest(components, {}, out_path)

    with open(out_path) as f:
        manifest = json.load(f)

    comp = manifest["components"][0]
    assert comp["name"] == "base"
    assert comp["component"] == "base"

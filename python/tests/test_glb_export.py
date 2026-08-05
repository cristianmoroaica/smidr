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

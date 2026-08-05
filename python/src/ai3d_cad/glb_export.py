"""Named-GLB export and manifest generation — mesh-level only.

Deliberately has no dependency on cadquery so it can be exercised in tests
(and reused by any caller) without a CAD kernel installed.
"""
from __future__ import annotations

import glob
import hashlib
import json
import os
from pathlib import Path

import numpy as np
import trimesh


def export_glb(components: dict, out_path) -> None:
    """Build a trimesh.Scene from named meshes and export it as GLB bytes.

    `components` maps node/geometry name -> trimesh.Trimesh. Node names are
    preserved in the exported GLB so downstream viewers can address parts
    individually.
    """
    if not components:
        raise ValueError("no components to export")

    scene = trimesh.Scene()
    for name, mesh in components.items():
        scene.add_geometry(mesh, node_name=name, geom_name=name)

    out_path = Path(out_path)
    out_path.parent.mkdir(parents=True, exist_ok=True)
    data = scene.export(file_type="glb")
    with open(out_path, "wb") as f:
        f.write(data)


def mesh_hash(mesh) -> str:
    """Deterministic sha256 hex digest over a mesh's vertex and face data."""
    h = hashlib.sha256()
    h.update(np.asarray(mesh.vertices, dtype=np.float64).tobytes())
    h.update(np.asarray(mesh.faces, dtype=np.int64).tobytes())
    return h.hexdigest()


def write_manifest(components: dict, spec_dims: dict, out_path) -> None:
    """Write manifest.json describing each component's bbox and mesh hash."""
    comp_entries = []
    for name, mesh in components.items():
        bounds = mesh.bounds
        comp_entries.append({
            "name": name,
            "bbox": [
                [float(v) for v in bounds[0]],
                [float(v) for v in bounds[1]],
            ],
            "mesh_hash": mesh_hash(mesh),
        })

    manifest = {
        "components": comp_entries,
        "dimensions": spec_dims,
    }

    out_path = Path(out_path)
    out_path.parent.mkdir(parents=True, exist_ok=True)
    with open(out_path, "w") as f:
        json.dump(manifest, f, indent=2)


def next_iteration(session_dir) -> int:
    """Return the next append-only iteration number for session_dir."""
    session_dir = str(session_dir)
    if not os.path.isdir(session_dir):
        return 1
    existing = glob.glob(os.path.join(session_dir, "iteration_*.glb"))
    return 1 + len(existing)


def export_iteration(session_dir, components: dict, spec_dims: dict) -> int:
    """Export the next iteration_NNN.glb + .manifest.json into session_dir.

    Returns the iteration number used. Never overwrites an existing pair.
    """
    session_dir = str(session_dir)
    n = next_iteration(session_dir)
    while True:
        glb_path = os.path.join(session_dir, f"iteration_{n:03d}.glb")
        if not os.path.exists(glb_path):
            break
        n += 1

    manifest_path = os.path.join(session_dir, f"iteration_{n:03d}.manifest.json")
    export_glb(components, glb_path)
    write_manifest(components, spec_dims, manifest_path)
    return n


def load_components(paths: dict) -> dict:
    """Load named STL files into trimesh.Trimesh objects, skipping failures."""
    result = {}
    for name, path in paths.items():
        try:
            if not os.path.exists(str(path)):
                continue
            mesh = trimesh.load_mesh(str(path), process=False)
            result[name] = mesh
        except Exception:
            continue
    return result

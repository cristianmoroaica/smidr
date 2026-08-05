"""Named-GLB export and manifest generation — mesh-level only.

Deliberately has no dependency on cadquery so it can be exercised in tests
(and reused by any caller) without a CAD kernel installed.
"""
from __future__ import annotations

import glob
import hashlib
import json
import os
import re
import sys
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


def write_manifest(components: dict, spec_dims: dict, out_path, sources: dict = None) -> None:
    """Write manifest.json describing each component's bbox and mesh hash.

    `sources` optionally maps a node name (a `components` key) to the source
    component name it was instanced from (for placement-driven multi-instance
    scenes). When absent, each entry's "component" defaults to its own name.
    """
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
            "component": sources.get(name, name) if sources else name,
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


def export_iteration(session_dir, components: dict, spec_dims: dict, sources: dict = None) -> int:
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
    write_manifest(components, spec_dims, manifest_path, sources=sources)
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


def load_placements(session_dir) -> dict:
    """Read <session_dir>/assembly/placements.json into name(lowercased) -> 4x4 np.ndarray.

    Returns {} when the file is missing, unreadable, or malformed.
    """
    path = os.path.join(str(session_dir), "assembly", "placements.json")
    try:
        with open(path) as f:
            data = json.load(f)
        result = {}
        for entry in data["placements"]:
            name = str(entry["name"]).lower()
            matrix = np.array(entry["matrix"], dtype=float).reshape(4, 4)
            result[name] = matrix
        return result
    except Exception:
        return {}


def apply_placements(components: dict, placements: dict) -> dict:
    """Return a new dict of components with matching placements applied.

    Component names (dict keys / GLB node names) are preserved verbatim.
    Matching against placement names is case-insensitive. Components with no
    matching placement keep an identity transform (unchanged, but still
    copied). Placement entries with no matching component are ignored.
    """
    if not placements:
        return components

    matched_placement_names = set()
    result = {}
    for name, mesh in components.items():
        mesh_copy = mesh.copy()
        matrix = placements.get(name.lower())
        if matrix is not None:
            mesh_copy.apply_transform(matrix)
            matched_placement_names.add(name.lower())
        else:
            print(
                f"placements.json: no placement for component {name!r}; using identity",
                file=sys.stderr,
            )
        result[name] = mesh_copy

    for placement_name in placements:
        if placement_name not in matched_placement_names:
            print(
                f"placements.json: no component for placement {placement_name!r}",
                file=sys.stderr,
            )

    return result


_TRAILING_INSTANCE_SUFFIX = re.compile(r"_\d+$")


def build_scene_nodes(components: dict, placements: dict) -> tuple:
    """Build a placement-driven, instance-aware set of GLB scene nodes.

    Returns `(nodes, sources)`: `nodes` maps GLB node name -> baked
    trimesh.Trimesh; `sources` maps that same node name -> the source
    component name (a key of `components`) it was instanced from.

    When `placements` is empty/falsy, this is a byte-for-byte legacy no-op:
    the input meshes are returned as-is (same objects, untouched) so the
    exported GLB matches today's output exactly.

    Otherwise each placement entry resolves to a component by (a) exact
    match on lowercased name, else (b) stripping one trailing `_<digits>`
    suffix and exact-matching again. A match duplicates the component mesh,
    bakes in the placement's 4x4 world matrix, and adds it as a node named
    after the placement (verbatim). An unmatched placement is skipped with a
    stderr warning. Any component never referenced by a placement is added
    once at identity, with the existing 'using identity' warning.

    Never mutates `components` or its meshes.
    """
    if not placements:
        return dict(components), {name: name for name in components}

    lower_to_name = {name.lower(): name for name in components}

    nodes = {}
    sources = {}
    referenced_components = set()

    for placement_name, matrix in placements.items():
        comp = lower_to_name.get(placement_name)
        if comp is None:
            stripped = _TRAILING_INSTANCE_SUFFIX.sub("", placement_name)
            comp = lower_to_name.get(stripped)

        if comp is None:
            print(
                f"placements.json: no component for placement {placement_name!r}; skipping node",
                file=sys.stderr,
            )
            continue

        if placement_name in nodes:
            continue

        mesh = components[comp].copy()
        mesh.apply_transform(matrix)
        nodes[placement_name] = mesh
        sources[placement_name] = comp
        referenced_components.add(comp)

    for name, mesh in components.items():
        if name in referenced_components:
            continue
        if name in nodes:
            continue
        print(
            f"placements.json: no placement for component {name!r}; using identity",
            file=sys.stderr,
        )
        nodes[name] = mesh.copy()
        sources[name] = name

    return nodes, sources

import sys
from pathlib import Path

import numpy as np

REPO_ROOT = Path(__file__).resolve().parents[2]
MCP_DIR = REPO_ROOT / "mcp"
SRC_DIR = REPO_ROOT / "python" / "src"
if str(MCP_DIR) not in sys.path:
    sys.path.insert(0, str(MCP_DIR))
if str(SRC_DIR) not in sys.path:
    sys.path.insert(0, str(SRC_DIR))

import server  # noqa: E402


def test_build_progress_line_format():
    assert server.build_progress_line("lid", "done") == "BUILD_COMPONENT: lid done"


def test_write_file_success_emits_build_progress_done(tmp_path, monkeypatch):
    monkeypatch.setattr(
        server,
        "run_cadquery_build",
        lambda *a, **k: {"success": True, "dimensions": "1x2x3", "stl_path": None},
    )
    monkeypatch.setattr(server, "_export_build_iteration", lambda *a, **k: None)

    result = server.handle_tool_call(
        "write_file",
        {"path": "components/lid/code.py", "content": "x=1"},
        str(tmp_path),
    )
    text = result[0]["text"]
    assert "BUILD_COMPONENT: lid done" in text


def test_write_file_failure_emits_build_progress_failed(tmp_path, monkeypatch):
    monkeypatch.setattr(
        server,
        "run_cadquery_build",
        lambda *a, **k: {"success": False, "error": "boom"},
    )

    result = server.handle_tool_call(
        "write_file",
        {"path": "components/lid/code.py", "content": "x=1"},
        str(tmp_path),
    )
    text = result[0]["text"]
    assert "BUILD_COMPONENT: lid failed" in text


def _find_tool(tools, tool_name):
    for tool in tools:
        if tool["name"] == tool_name:
            return tool
    raise AssertionError(f"tool {tool_name!r} not found")


def test_ask_question_schema_has_optional_options():
    tool = _find_tool(server.SPEC_TOOLS, "ask_question")
    options_prop = tool["inputSchema"]["properties"]["options"]
    assert options_prop["type"] == "array"
    assert options_prop["items"]["type"] == "string"
    assert tool["inputSchema"]["required"] == ["question"]


def test_ask_clarification_schemas_have_options():
    for tools in (server.BUILD_TOOLS, server.REFINE_TOOLS):
        tool = _find_tool(tools, "ask_clarification")
        options_prop = tool["inputSchema"]["properties"]["options"]
        assert options_prop["type"] == "array"
        assert options_prop["items"]["type"] == "string"
        assert tool["inputSchema"]["required"] == ["question"]


def test_ask_question_handler_passes_options_through(tmp_path):
    result = server.handle_tool_call(
        "ask_question",
        {"question": "How wide?", "options": ["20mm", "40mm"]},
        str(tmp_path),
    )
    text = result[0]["text"]
    assert "How wide?" in text
    assert "20mm" in text
    assert "40mm" in text


def test_ask_question_handler_without_options(tmp_path):
    result = server.handle_tool_call(
        "ask_question",
        {"question": "How wide?"},
        str(tmp_path),
    )
    assert result[0]["text"] == "Question delivered to user: How wide?"


def test_request_phase_change_present_in_all_phase_tool_lists():
    for tools in (server.SPEC_TOOLS, server.BUILD_TOOLS, server.REFINE_TOOLS):
        tool = _find_tool(tools, "request_phase_change")
        assert tool["inputSchema"]["required"] == ["target", "reason"]
        assert tool["inputSchema"]["properties"]["target"]["enum"] == [
            "spec",
            "build",
            "refine",
        ]


def test_request_phase_change_handler_returns_delivery_text():
    result = server.handle_tool_call(
        "request_phase_change",
        {"target": "build", "reason": "needs a functional change"},
        None,
    )
    assert (
        result[0]["text"]
        == "Phase change request delivered to user: → build (needs a functional change)"
    )


def test_export_build_iteration_applies_placements(tmp_path):
    """Exercises the real glb_export.export_iteration path (not a mock) with
    two components — one placed, one left at identity — and asserts both
    named nodes survive into the exported GLB, since part-click / ghost /
    dimensions in the frontend depend on that."""
    import json as _json
    import trimesh

    session_dir = tmp_path / "session"
    lid_dir = session_dir / "components" / "lid"
    base_dir = session_dir / "components" / "base"
    lid_dir.mkdir(parents=True)
    base_dir.mkdir(parents=True)
    trimesh.creation.box(extents=(10, 10, 10)).export(str(lid_dir / "result.stl"))
    trimesh.creation.box(extents=(10, 10, 10)).export(str(base_dir / "result.stl"))

    assembly_dir = session_dir / "assembly"
    assembly_dir.mkdir(parents=True)
    matrix = [1.0, 0.0, 0.0, 0.0,
              0.0, 1.0, 0.0, 0.0,
              0.0, 0.0, 1.0, 25.0,
              0.0, 0.0, 0.0, 1.0]
    (assembly_dir / "placements.json").write_text(
        _json.dumps({"placements": [{"name": "lid", "matrix": matrix}]})
    )

    n = server._export_build_iteration(str(session_dir), "build", None, None)
    assert n is not None

    glb_path = session_dir / f"iteration_{n:03d}.glb"
    assert glb_path.exists()

    loaded = trimesh.load(str(glb_path), file_type="glb")
    names = set(loaded.geometry.keys()) | set(loaded.graph.nodes_geometry)
    # Both named nodes present — placement application must not drop or
    # rename components.
    assert "lid" in names
    assert "base" in names

    original_lid = trimesh.load_mesh(str(lid_dir / "result.stl"), process=False)
    original_base = trimesh.load_mesh(str(base_dir / "result.stl"), process=False)

    placed_lid = loaded.geometry["lid"]
    placed_base = loaded.geometry["base"]

    assert not np.allclose(placed_lid.vertices, original_lid.vertices)
    assert np.allclose(placed_lid.vertices[:, 2], original_lid.vertices[:, 2] + 25.0)
    # Unplaced component keeps identity transform.
    assert np.allclose(placed_base.vertices, original_base.vertices)

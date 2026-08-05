import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[2]
MCP_DIR = REPO_ROOT / "mcp"
if str(MCP_DIR) not in sys.path:
    sys.path.insert(0, str(MCP_DIR))

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

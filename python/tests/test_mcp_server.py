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

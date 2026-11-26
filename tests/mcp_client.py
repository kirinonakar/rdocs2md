"""Integration test using the official MCP Python SDK 1.x and the release executable."""
import asyncio
import json
from pathlib import Path
import tempfile
from mcp import ClientSession, StdioServerParameters
from mcp.client.stdio import stdio_client

async def check():
    root = Path(__file__).resolve().parents[1]
    exe = root / "target/release/rdocs2md.exe"
    with tempfile.TemporaryDirectory(dir=root / "target") as directory:
        work = Path(directory)
        (work / "sample.rtf").write_bytes(br"{\rtf1\ansi Page one\page Page two}")
        params = StdioServerParameters(command=str(exe), args=["mcp","--root",str(work)])
        async with stdio_client(params) as (read, write):
            async with ClientSession(read, write) as client:
                info = await client.initialize()
                assert info.serverInfo.name == "rdocs2md"
                await client.send_ping()
                tools = await client.list_tools()
                assert {t.name for t in tools.tools} == {
                    "list_supported_formats", "inspect_document", "convert_document",
                    "extract_document_range", "convert_documents", "search_document",
                    "search_documents",
                }
                async def call(name, args):
                    res = await client.call_tool(name, args)
                    assert not res.isError, res
                    return res.content[0].text
                assert len(json.loads(await call("list_supported_formats", {}))) == 14
                doc = json.loads(await call("inspect_document", {"path":"sample.rtf"}))
                assert len(doc["sections"]) == 2
                res = await call("extract_document_range", {"path":"sample.rtf","range":"2"})
                assert "Page two" in res and "Page one" not in res
                search = json.loads(await call("search_document", {
                    "path":"sample.rtf", "query":"TWO", "range":"2",
                }))
                assert search["matches"][0]["section_index"] == 2
                assert search["matches"][0]["before"] == "Page "
                (work / "nested").mkdir()
                (work / "nested/copy.rtf").write_bytes((work / "sample.rtf").read_bytes())
                batch = json.loads(await call("convert_documents", {
                    "paths":["*.rtf", "bad.doc"], "recursive":True, "output_dir":"batch",
                }))
                assert batch["succeeded"] == 2 and batch["failed"] == 1
                assert (work / "batch/copy.md").is_file()
                await call("convert_document", {"path":"sample.rtf","output_path":"result.md"})
                assert "Page one" in (work / "result.md").read_text(encoding="utf-8")
                denied = await client.call_tool("inspect_document", {"path":str(root / "Cargo.toml")})
                assert denied.isError and "outside" in denied.content[0].text
                (work / "large.rtf").write_bytes(br"{\rtf1 " + b"x" * 600000 + b"}")
                big = await client.call_tool("convert_document", {"path":"large.rtf"})
                assert big.isError and "512 KiB" in big.content[0].text
                await call("convert_document", {"path":"large.rtf","output_path":"large.md"})
                print(f"{info.protocolVersion} · {len(tools.tools)} tools · range, paths, large output passed")

async def check_outside_root():
    project = Path(__file__).resolve().parents[1]
    exe = project / "target/release/rdocs2md.exe"
    with tempfile.TemporaryDirectory(dir=project / "target") as directory:
        base = Path(directory)
        root = base / "root"
        root.mkdir()
        image_doc = br"{\rtf1 Test{\pict 47494638396101000100800000000000ffffff21f90401000000002c00000000010001000002024401003b}}"
        (root / "inside.rtf").write_bytes(image_doc)
        outside = base / "outside.rtf"
        outside.write_bytes(image_doc)
        for allowed in (False, True):
            flags = ["--allow-outside-root"] if allowed else []
            server = StdioServerParameters(command=str(exe), args=["mcp", *flags, "--root", str(root)])
            async with stdio_client(server) as (read, write), ClientSession(read, write) as client:
                await client.initialize()
                cases = [
                    ("inspect_document", {"path": str(outside)}),
                    ("extract_document_range", {"path": "../outside.rtf", "range": "1"}),
                    ("convert_document", {"path": "inside.rtf", "output_path": "../result.md"}),
                    ("convert_document", {"path": "inside.rtf", "image_dir": str(base / "images")}),
                ]
                for name, args in cases:
                    result = await client.call_tool(name, args)
                    assert result.isError == (not allowed), (name, result)
                    if not allowed:
                        assert "outside" in result.content[0].text
                assert (base / "result.md").is_file() == allowed
                assert (base / "images/image-1.gif").is_file() == allowed
        assert "Test" in (base / "result.md").read_text(encoding="utf-8")
        print("Default confinement and opt-in external reads, ranges, Markdown and images passed")

if __name__ == "__main__":
    asyncio.run(check())
    asyncio.run(check_outside_root())

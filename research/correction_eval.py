"""Compare how 2 local MCP servers handle explicitly corrected decisions."""

import argparse
import asyncio
import json
import os
from contextlib import asynccontextmanager
from pathlib import Path
from tempfile import TemporaryDirectory

from research.locomo_retrieval.run import Store

CASES = (
    ("repo/demo/build/runner", "Make", "Ninja", "Bazel", "CMake"),
    ("repo/demo/api/encoding", "YAML", "JSON", "TOML", "CSV"),
    ("repo/demo/tests/fixture", "small", "medium", "large", "tiny"),
)
RPC_TIMEOUT = 5


class Mcp:
    def __init__(self, process):
        self.process = process
        self.next_id = 0

    async def send(self, message):
        self.process.stdin.write((json.dumps({"jsonrpc": "2.0", **message}) + "\n").encode())
        await self.process.stdin.drain()

    async def request(self, method, params):
        self.next_id += 1
        request_id = self.next_id

        async def exchange():
            await self.send({"id": request_id, "method": method, "params": params})
            while line := await self.process.stdout.readline():
                reply = json.loads(line)
                if reply.get("id") != request_id:
                    continue
                if "error" in reply:
                    raise RuntimeError(f"{method}: {reply['error']}")
                return reply["result"]
            raise RuntimeError(f"{method}: MCP closed before replying")

        return await asyncio.wait_for(exchange(), RPC_TIMEOUT)

    async def tool(self, name, args, *, allow_error=False):
        result = await self.request("tools/call", {"name": name, "arguments": args})
        if result.get("isError") and not allow_error:
            raise RuntimeError(f"{name}: {result}")
        return result


@asynccontextmanager
async def session(argv, cwd, env):
    process = await asyncio.create_subprocess_exec(
        *argv, cwd=cwd, env=env, stdin=asyncio.subprocess.PIPE, stdout=asyncio.subprocess.PIPE,
    )
    mcp = Mcp(process)
    try:
        info = await mcp.request("initialize", {
            "protocolVersion": "2024-11-05", "capabilities": {},
            "clientInfo": {"name": "correction-eval", "version": "1"},
        })
        if info["protocolVersion"] != "2024-11-05":
            raise RuntimeError(f"unexpected MCP protocol: {info['protocolVersion']}")
        await asyncio.wait_for(mcp.send({"method": "notifications/initialized"}), RPC_TIMEOUT)
        yield mcp
    finally:
        process.stdin.close()
        try:
            await asyncio.wait_for(process.wait(), RPC_TIMEOUT)
        except asyncio.TimeoutError:
            if process.returncode is None:
                process.kill()
            await process.wait()


def report(current_only, ordered_history, stale_write_rejected, values_after_overlap):
    return {"current_only": current_only, "ordered_history": ordered_history,
            "stale_write_rejected": stale_write_rejected, "values_after_overlap": values_after_overlap}


async def atticdb_case(a, b, key, initial, corrected, writer_a, writer_b):
    def args(value):
        return {"key": key, "content": value}

    async def current(mcp):
        result = await mcp.tool("fact_get", {"key": key})
        return result["content"][0]["text"]

    await a.tool("fact_set", args(initial))
    await a.tool("fact_set", args(corrected))
    handoff = await current(b)
    history = await b.tool("fact_history", {"key": key})
    revisions = history["structuredContent"]["revisions"]
    await current(a)
    await a.tool("fact_set", args(writer_a))
    stale = await b.tool("fact_set", args(writer_b), allow_error=True)
    rejected = stale.get("isError") is True and stale["content"][0]["text"].startswith("conflict:")
    return report(handoff == corrected, [r["content"] for r in revisions] == [corrected, initial],
                  rejected, [await current(b)])


async def reference_case(a, b, key, initial, corrected, writer_a, writer_b):
    async def current(mcp):
        nodes = await mcp.tool("open_nodes", {"names": [key]})
        [node] = nodes["structuredContent"]["entities"]
        return node["observations"]

    async def replace(mcp, previous, value):
        add = await mcp.tool("add_observations", {
            "observations": [{"entityName": key, "contents": [value]}],
        }, allow_error=True)
        if add.get("isError"):
            return False
        await mcp.tool("delete_observations", {
            "deletions": [{"entityName": key, "observations": [previous]}],
        })
        return True

    await a.tool("create_entities", {
        "entities": [{"name": key, "entityType": "decision", "observations": [initial]}],
    })
    if not await replace(a, initial, corrected):
        raise RuntimeError("reference server rejected the initial correction")
    handoff = await current(b)
    await current(a)
    if not await replace(a, corrected, writer_a):
        raise RuntimeError("reference server rejected the 1st concurrent edit")
    stale_accepted = await replace(b, corrected, writer_b)
    return report(handoff == [corrected], None, not stale_accepted, sorted(await current(b)))


async def compare(atticdb: str, reference: str) -> list[dict]:
    atticdb, reference = str(Path(atticdb).resolve(strict=True)), Path(reference).resolve(strict=True)
    package = json.loads((reference.parent.parent / "package.json").read_text(encoding="utf-8"))
    if (package["name"], package["version"]) != ("@modelcontextprotocol/server-memory", "2.0.0"):
        raise ValueError("use @modelcontextprotocol/server-memory@2.0.0")
    with TemporaryDirectory(prefix="mcp-correction-") as root, Store(atticdb) as store:
        token = Path(root) / "token"
        token.touch(mode=0o600)
        config = Path(store.root) / "zh" / "atticdb.json"
        token.write_text(Store._token(str(config)), encoding="utf-8")
        adb = [atticdb, "mcp", "--url", f"unix://{store.root}/s", "--token-file", str(token)]
        ref = ["node", str(reference)]
        # Neither child inherits live MCP settings or credentials.
        env = {"PATH": os.environ["PATH"], "HOME": root, "ATTICDB_HOME": str(config.parent),
               "ATTICDB_NO_UPDATE_CHECK": "1", "MEMORY_FILE_PATH": str(Path(root) / "memory.jsonl")}
        async with (
            session(adb + ["--agent", "a"], root, env) as aa,
            session(adb + ["--agent", "b"], root, env) as ab,
            session(ref, root, env) as ra,
            session(ref, root, env) as rb,
        ):
            return [
                {"key": case[0], "atticdb": await atticdb_case(aa, ab, *case),
                 "reference": await reference_case(ra, rb, *case)}
                for case in CASES
            ]


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--atticdb", required=True)
    parser.add_argument("--reference", required=True, help="path to the reference server's dist/index.js")
    args = parser.parse_args()
    print(json.dumps(asyncio.run(compare(args.atticdb, args.reference)), indent=2))

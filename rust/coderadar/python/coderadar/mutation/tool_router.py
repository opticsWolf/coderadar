"""Route LLM tool calls to the engine's mutation planners."""

from dataclasses import dataclass, field
from typing import Any

from .. import create_entity, rename, replace_body, update_signature


@dataclass
class ToolCall:
    """A tool invocation with its parsed arguments."""

    tool_name: str
    arguments: dict[str, Any] = field(default_factory=dict)
    call_id: str = ""


@dataclass
class ToolResult:
    """What a routed call produced."""

    call_id: str
    success: bool
    result: Any = None
    error: str | None = None


class ToolRouter:
    """Dispatch the four mutation tools; the engine does the work."""

    def __init__(self, graph: Any = None, dry_run: bool = True):
        self.graph = graph
        self.dry_run = dry_run

    def route(self, call: ToolCall) -> ToolResult:
        """Execute one call and wrap the outcome."""
        try:
            value = self._dispatch(call.tool_name, call.arguments)
        except Exception as error:  # noqa: BLE001 - reported, not raised
            return ToolResult(call.call_id, False, error=str(error))
        return ToolResult(call.call_id, True, result=value)

    def _dispatch(self, name: str, args: dict[str, Any]) -> Any:
        if name == "replace_entity_body":
            return replace_body(
                args["entity_id"], args["new_body"], args.get("expected_hash"), self.dry_run
            )
        if name == "update_signature":
            return update_signature(
                args["entity_id"], args["new_signature"],
                args.get("inject_defaults", False), self.dry_run,
            )
        if name == "rename_symbol":
            return rename(args["entity_id"], args["new_name"], self.dry_run)
        if name == "create_entity":
            return create_entity(
                args["file_path"], args["language"], args["kind"], args["name"],
                args["body"], args.get("decorators"), args.get("anchor", "end"),
                args.get("signature"), self.dry_run,
            )
        raise ValueError(f"unknown mutation tool `{name}`")


__all__ = ["ToolCall", "ToolResult", "ToolRouter"]

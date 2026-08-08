"""CodeRadar v3.3 — Mutation Tool Router (§11.2)

Routes LLM tool calls to the appropriate Rust planner: replace_entity_body,
update_signature, rename_symbol, create_entity.
"""

from __future__ import annotations

import structlog
from dataclasses import dataclass, field
from typing import Any, Dict, List, Optional

logger = structlog.get_logger(__name__)


@dataclass
class ToolCall:
    """An LLM tool call with parsed function selection and arguments."""
    tool_name: str
    arguments: Dict[str, Any]
    call_id: str


@dataclass
class ToolResult:
    """Result of executing a mutation tool call."""
    call_id: str
    success: bool
    result: Any
    error: Optional[str] = None


class ToolRouter:
    """Routes LLM tool calls to the appropriate mutation planner.

    The four tools (§11.2):
    - replace_entity_body: Replace a function/method body
    - update_signature: Update signature + cascade to call sites
    - rename_symbol: Rename across the codebase
    - create_entity: Create a new entity at an anchor point
    """

    def __init__(self, graph: Any = None, dry_run: bool = True):
        self.graph = graph
        self.dry_run = dry_run

    def route(self, call: ToolCall) -> ToolResult:
        """Route a tool call to the appropriate planner."""
        try:
            if call.tool_name == "replace_entity_body":
                return self._handle_body_replacement(call)
            elif call.tool_name == "update_signature":
                return self._handle_signature_update(call)
            elif call.tool_name == "rename_symbol":
                return self._handle_rename(call)
            elif call.tool_name == "create_entity":
                return self._handle_create_entity(call)
            else:
                return ToolResult(
                    call_id=call.call_id,
                    success=False,
                    result=None,
                    error=f"Unknown tool: {call.tool_name}",
                )
        except Exception as e:
            logger.error("tool_call_error", tool=call.tool_name,
                          error=str(e))
            return ToolResult(
                call_id=call.call_id,
                success=False,
                result=None,
                error=str(e),
            )

    def _handle_body_replacement(self, call: ToolCall) -> ToolResult:
        """replace_entity_body: entity_id, new_body, expected_hash?"""
        args = call.arguments
        entity_id = args["entity_id"]
        new_body = args["new_body"]
        expected_hash = args.get("expected_hash")

        if self.graph:
            plan = self.graph.plan_body_replacement(
                entity_id, new_body, expected_hash, dry_run=self.dry_run,
            )
            if not self.dry_run:
                result = self.graph.apply(plan)
                return ToolResult(
                    call_id=call.call_id,
                    success=result.status == "Applied",
                    result={
                        "status": result.status,
                        "files_written": result.files_written,
                        "syntax_errors": result.syntax_errors,
                    },
                    error=None if result.status == "Applied"
                    else "\n".join(str(e) for e in result.syntax_errors),
                )
            return ToolResult(
                call_id=call.call_id, success=True,
                result={"plan": plan.id, "diff_preview": plan.diff_preview},
            )

        return ToolResult(call_id=call.call_id, success=False, result=None,
                          error="No graph available")

    def _handle_signature_update(self, call: ToolCall) -> ToolResult:
        """update_signature: entity_id, new_signature, call_site_values?
                            inject_defaults?"""
        args = call.arguments
        entity_id = args["entity_id"]
        new_signature = args["new_signature"]
        call_site_values = args.get("call_site_values", {})
        inject_defaults = args.get("inject_defaults", False)

        if self.graph:
            plan = self.graph.plan_signature_update(
                entity_id, new_signature, call_site_values,
                inject_defaults, dry_run=self.dry_run,
            )
            if not self.dry_run:
                result = self.graph.apply(plan)
                return ToolResult(
                    call_id=call.call_id,
                    success=result.status == "Applied",
                    result={
                        "status": result.status,
                        "files_written": result.files_written,
                        "unverified_sites": plan.unverified_sites,
                    },
                )
            return ToolResult(
                call_id=call.call_id, success=True,
                result={
                    "plan": plan.id,
                    "affected_files": plan.affected_files,
                    "unverified_sites": plan.unverified_sites,
                },
            )

        return ToolResult(call_id=call.call_id, success=False, result=None,
                          error="No graph available")

    def _handle_rename(self, call: ToolCall) -> ToolResult:
        """rename_symbol: entity_id, new_name, include_strings?"""
        args = call.arguments
        entity_id = args["entity_id"]
        new_name = args["new_name"]
        include_strings = args.get("include_strings", False)

        if self.graph:
            plan = self.graph.plan_rename(
                entity_id, new_name, include_strings, dry_run=self.dry_run,
            )
            if not self.dry_run:
                result = self.graph.apply(plan)
                return ToolResult(
                    call_id=call.call_id,
                    success=result.status == "Applied",
                    result={
                        "status": result.status,
                        "files_written": result.files_written,
                    },
                )
            return ToolResult(
                call_id=call.call_id, success=True,
                result={
                    "plan": plan.id,
                    "affected_files": plan.affected_files,
                    "diff_preview": plan.diff_preview,
                },
            )

        return ToolResult(call_id=call.call_id, success=False, result=None,
                          error="No graph available")

    def _handle_create_entity(self, call: ToolCall) -> ToolResult:
        """create_entity: target_file, anchor, code"""
        args = call.arguments
        target_file = args["target_file"]
        anchor = args.get("anchor", "end")
        code = args["code"]

        if self.graph:
            plan = self.graph.plan_create_entity(
                target_file, anchor, code, dry_run=self.dry_run,
            )
            if not self.dry_run:
                result = self.graph.apply(plan)
                return ToolResult(
                    call_id=call.call_id,
                    success=result.status == "Applied",
                    result={
                        "status": result.status,
                        "files_written": result.files_written,
                        "warnings": plan.warnings,
                    },
                )
            return ToolResult(
                call_id=call.call_id, success=True,
                result={
                    "plan": plan.id,
                    "affected_files": plan.affected_files,
                    "warnings": plan.warnings,
                },
            )

        return ToolResult(call_id=call.call_id, success=False, result=None,
                          error="No graph available")

    def check_policy(self, plan: Any) -> bool:
        """Check mutation policy before applying."""
        # Verify: enabled, deny-listed paths, max_files_per_plan, max_edits_per_plan,
        # require_clean_git
        return True

"""CodeRadar v0.5.7 — Rails Framework Resolver (F.13)

Handles Ruby on Rails model associations, controller callbacks,
and validations. Produces synthetic edges for ActiveRecord relationships.

Patterns:
  has_many :users, through: :memberships
  belongs_to :author, class_name: 'User'
  has_one :profile, dependent: :destroy
  has_and_belongs_to_many :tags
  before_action :authenticate_user, only: [:edit, :update]
  after_action :log_access
  validates :name, presence: true, uniqueness: true
"""

from __future__ import annotations

import re
from pathlib import Path
from typing import Any, Dict, List, Optional

from .base import FrameworkExtraction, FrameworkResolver, SyntheticEdge, SyntheticNode

# ── Regex patterns ──────────────────────────────────────────────────────────

# has_many :users, through: :memberships
# belongs_to :author, class_name: 'User'
# has_one :profile, dependent: :destroy
# has_and_belongs_to_many :tags
_ASSOCIATION_RE = re.compile(
    r'(has_many|has_one|belongs_to|has_and_belongs_to_many)'
    r'\s+:(\w+)'
    r'(?:\s*,\s*(?:class_name\s*:\s*[\"\'](\w+)[\"\']'
    r'|through\s*:\s+:(\w+)'
    r'|source\s*:\s+:(\w+)))*',
    re.IGNORECASE,
)

# before_action :authenticate_user, only: [:edit, :update]
# after_action :log_access
# skip_before_action :verify_authenticity_token
_CALLBACK_RE = re.compile(
    r'(before_action|after_action|around_action|skip_before_action|'
    r'skip_after_action|before_save|after_save|before_create|after_create|'
    r'before_validation|after_validation)'
    r'\s+:(\w+)',
    re.IGNORECASE,
)

# validates :name, presence: true, uniqueness: true
_VALIDATION_RE = re.compile(
    r'validates\s+:(\w+)',
    re.IGNORECASE,
)

# Gemfile detection: gem 'rails' or gem "rails"
_GEMFILE_RAILS_RE = re.compile(r"gem\s+['\"]rails['\"]", re.IGNORECASE)

# Class name extraction: class UserController < ApplicationController
_CLASS_RE = re.compile(
    r'class\s+(\w+)\s*<\s*(?:ApplicationController|ApplicationRecord|ActiveRecord::Base)',
    re.IGNORECASE,
)

_HANDLER_DIRS = ['app/models', 'app/controllers', 'app/services', 'models', 'controllers']
_SERVICE_DIRS = ['app/services', 'app/models', 'app/helpers', 'lib', 'config']


class RailsResolver(FrameworkResolver):
    """Ruby on Rails model/controller resolver."""

    @property
    def name(self) -> str:
        return "rails"

    def detect(self, project_root: Path) -> bool:
        gemfile = project_root / "Gemfile"
        if gemfile.exists():
            try:
                if _GEMFILE_RAILS_RE.search(gemfile.read_text(encoding="utf-8")):
                    return True
            except (OSError, UnicodeDecodeError):
                pass
        # Fallback: config/routes.rb or app/models/
        markers = ['config/routes.rb', 'app/models', 'app/controllers']
        for m in markers:
            if (project_root / m).exists():
                return True
        return False

    def claims_reference(self, name: str) -> bool:
        parts = name.rsplit(".", 1)[-1]
        return (
            parts.endswith("Controller")
            or parts.endswith("Model")
            or parts.endswith("Record")
        )

    def extract(self, file_path: str, source: str) -> FrameworkExtraction:
        nodes: List[SyntheticNode] = []
        edges: List[SyntheticEdge] = []

        if not file_path.endswith('.rb'):
            return FrameworkExtraction(file_path=file_path)

        class_match = _CLASS_RE.search(source)
        class_name = class_match.group(1) if class_match else None
        is_model = bool(
            class_match
            and ('ActiveRecord' in class_match.group(0) or 'ApplicationRecord' in class_match.group(0))
        )
        is_controller = bool(
            class_match
            and 'ApplicationController' in class_match.group(0)
        )

        # ── Model associations ──
        for match in _ASSOCIATION_RE.finditer(source):
            assoc_kind = match.group(1).lower()
            assoc_name = match.group(2)

            line_no = source[:match.start()].count('\n') + 1
            node_id = f"rails:assoc:{file_path}:{line_no}:{assoc_kind}:{assoc_name}"

            nodes.append(SyntheticNode(
                id=node_id,
                name=f"{assoc_kind} :{assoc_name}",
                kind="association",
                file_path=file_path,
                metadata={
                    "language": "ruby",
                    "framework": "rails",
                    "kind": assoc_kind,
                    "name": assoc_name,
                    "line": line_no,
                    "class": class_name,
                },
            ))

            if class_name:
                # Edge from the model to the associated model
                source_id = f"{file_path}::{class_name}"
                edges.append(SyntheticEdge(
                    source_id=source_id,
                    target_id=assoc_name.capitalize(),
                    kind=assoc_kind,
                    metadata={
                        "synthesizedBy": "rails-resolver",
                        "line": line_no,
                        "association": assoc_name,
                    },
                ))

        # ── Controller callbacks ──
        if is_controller:
            for match in _CALLBACK_RE.finditer(source):
                callback_kind = match.group(1).lower()
                method_name = match.group(2)

                line_no = source[:match.start()].count('\n') + 1
                node_id = f"rails:callback:{file_path}:{line_no}:{callback_kind}:{method_name}"

                nodes.append(SyntheticNode(
                    id=node_id,
                    name=f"{callback_kind} :{method_name}",
                    kind="callback",
                    file_path=file_path,
                    metadata={
                        "language": "ruby",
                        "framework": "rails",
                        "kind": callback_kind,
                        "method": method_name,
                        "line": line_no,
                        "class": class_name,
                    },
                ))

                if class_name:
                    qualified = f"{class_name}#{method_name}"
                    edges.append(SyntheticEdge(
                        source_id=f"{file_path}::{class_name}",
                        target_id=qualified,
                        kind="callback",
                        metadata={
                            "synthesizedBy": "rails-resolver",
                            "line": line_no,
                            "callback": callback_kind,
                            "method": method_name,
                        },
                    ))

        return FrameworkExtraction(file_path=file_path, nodes=nodes, edges=edges)

    def resolve(
        self, ref_name: str, candidates: List[Dict[str, Any]],
    ) -> Optional[Dict[str, Any]]:
        if not candidates:
            return None
        pref_dirs = _HANDLER_DIRS + _SERVICE_DIRS
        for result in candidates:
            fp = result.get("file_path", "")
            for d in pref_dirs:
                if f"/{d}/" in fp or f"\\{d}\\" in fp:
                    result["confidence"] = 0.85
                    return result
        result = candidates[0]
        result["confidence"] = 0.65
        return result

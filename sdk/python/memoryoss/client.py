# SPDX-License-Identifier: AGPL-3.0-only
# Copyright (C) 2026 memoryOSS Contributors
"""memoryOSS Python SDK — Memory for AI Agents."""

from __future__ import annotations

from dataclasses import dataclass, field
from datetime import datetime
from typing import Any, Optional

import httpx


@dataclass
class Memory:
    id: str
    content: str
    tags: list[str] = field(default_factory=list)
    agent: Optional[str] = None
    session: Optional[str] = None
    namespace: Optional[str] = None
    memory_type: str = "semantic"
    version: int = 1
    score: Optional[float] = None
    provenance: list[str] = field(default_factory=list)
    created_at: Optional[str] = None
    updated_at: Optional[str] = None


class MemoryOSSError(Exception):
    def __init__(self, status: int, message: str):
        self.status = status
        self.message = message
        super().__init__(f"[{status}] {message}")


class MemoryOSSClient:
    """Client for the memoryOSS Agent Memory Database."""

    def __init__(
        self,
        url: str = "https://localhost:8000",
        api_key: Optional[str] = None,
        verify_ssl: bool = True,
    ):
        self._url = url.rstrip("/")
        self._api_key = api_key
        self._token: Optional[str] = None
        self._http = httpx.Client(verify=verify_ssl, timeout=30.0)

    def close(self):
        self._http.close()

    def __enter__(self):
        return self

    def __exit__(self, *args):
        self.close()

    def _headers(self) -> dict[str, str]:
        headers = {"Content-Type": "application/json"}
        if self._token:
            headers["Authorization"] = f"Bearer {self._token}"
        return headers

    def _request(self, method: str, path: str, **kwargs) -> Any:
        resp = self._http.request(
            method, f"{self._url}{path}", headers=self._headers(), **kwargs
        )
        if resp.status_code >= 400:
            try:
                body = resp.json()
                msg = body.get("error", resp.text)
            except Exception:
                msg = resp.text
            raise MemoryOSSError(resp.status_code, msg)
        return resp.json()

    def authenticate(self) -> str:
        """Exchange API key for a JWT token."""
        if not self._api_key:
            raise MemoryOSSError(0, "no API key set")
        data = self._request("POST", "/v1/auth/token", json={"api_key": self._api_key})
        self._token = data["token"]
        return self._token

    def store(
        self,
        content: str,
        *,
        tags: Optional[list[str]] = None,
        agent: Optional[str] = None,
        session: Optional[str] = None,
        namespace: Optional[str] = None,
        memory_type: Optional[str] = None,
    ) -> dict:
        """Store a new memory. Returns {"id": ..., "version": ...}."""
        body: dict[str, Any] = {"content": content}
        if tags:
            body["tags"] = tags
        if agent:
            body["agent"] = agent
        if session:
            body["session"] = session
        if namespace:
            body["namespace"] = namespace
        if memory_type:
            body["memory_type"] = memory_type
        return self._request("POST", "/v1/store", json=body)

    def store_batch(
        self,
        memories: list[dict],
    ) -> list[dict]:
        """Store multiple memories at once. Returns list of {"id": ..., "version": ...}."""
        data = self._request("POST", "/v1/store/batch", json={"memories": memories})
        return data["stored"]

    def recall(
        self,
        query: str,
        *,
        limit: int = 10,
        agent: Optional[str] = None,
        session: Optional[str] = None,
        namespace: Optional[str] = None,
        memory_type: Optional[str] = None,
        tags: Optional[list[str]] = None,
    ) -> list[Memory]:
        """Search memories with semantic + keyword matching."""
        body: dict[str, Any] = {"query": query, "limit": limit}
        if agent:
            body["agent"] = agent
        if session:
            body["session"] = session
        if namespace:
            body["namespace"] = namespace
        if memory_type:
            body["memory_type"] = memory_type
        if tags:
            body["tags"] = tags
        data = self._request("POST", "/v1/recall", json=body)
        return [
            Memory(
                id=m["memory"]["id"],
                content=m["memory"]["content"],
                tags=m["memory"].get("tags", []),
                agent=m["memory"].get("agent"),
                session=m["memory"].get("session"),
                namespace=m["memory"].get("namespace"),
                memory_type=m["memory"].get("memory_type", "semantic"),
                version=m["memory"].get("version", 1),
                score=m.get("score"),
                provenance=m.get("provenance", []),
                created_at=m["memory"].get("created_at"),
                updated_at=m["memory"].get("updated_at"),
            )
            for m in data["memories"]
        ]

    def update(
        self,
        id: str,
        *,
        content: Optional[str] = None,
        tags: Optional[list[str]] = None,
        memory_type: Optional[str] = None,
    ) -> dict:
        """Update an existing memory. Returns {"id": ..., "version": ..., "updated_at": ...}."""
        body: dict[str, Any] = {"id": id}
        if content is not None:
            body["content"] = content
        if tags is not None:
            body["tags"] = tags
        if memory_type is not None:
            body["memory_type"] = memory_type
        return self._request("PATCH", "/v1/update", json=body)

    def forget(
        self,
        *,
        ids: Optional[list[str]] = None,
        agent: Optional[str] = None,
        session: Optional[str] = None,
        namespace: Optional[str] = None,
        tags: Optional[list[str]] = None,
        before: Optional[str] = None,
    ) -> int:
        """Delete memories. Returns count of deleted memories."""
        body: dict[str, Any] = {}
        if ids:
            body["ids"] = ids
        if agent:
            body["agent"] = agent
        if session:
            body["session"] = session
        if namespace:
            body["namespace"] = namespace
        if tags:
            body["tags"] = tags
        if before:
            body["before"] = before
        data = self._request("DELETE", "/v1/forget", json=body)
        return data["deleted"]

    def health(self) -> dict:
        """Check server health."""
        return self._request("GET", "/health")


def connect(
    url: str = "https://localhost:8000",
    api_key: Optional[str] = None,
    verify_ssl: bool = True,
) -> MemoryOSSClient:
    """Create and optionally authenticate a memoryOSS client."""
    client = MemoryOSSClient(url=url, api_key=api_key, verify_ssl=verify_ssl)
    if api_key:
        client.authenticate()
    return client

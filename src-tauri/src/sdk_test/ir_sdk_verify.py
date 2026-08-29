#!/usr/bin/env python3
"""IR 层 3×3 SDK 合规性验证。

用官方 SDK（openai / anthropic）验证 Rust IR 层的转换输出：
- req:    IR → 三种客户端格式请求体（用 httpx.MockTransport 捕获 SDK 实际发出的请求体）
- stream: IR → 三种客户端格式 SSE 帧（喂给 SDK 流解析器，验证可被官方 SDK 消费）
- parse:  三种上游格式 chunk 序列 → IR 事件（验证 IR 解析出的结构化事件合法）

用法：cargo test --lib sdk_fixtures && python3 scripts/ir_sdk_verify.py
"""

import json
import sys
from pathlib import Path

import httpx

# ── SDK 环境 ────────────────────────────────────────────────────────
# 用本目录 venv 的官方 SDK：python3 -m venv .venv && .venv/bin/pip install openai anthropic
VENV = Path(__file__).parent / ".venv" / "bin" / "python"
if not VENV.exists():
    sys.exit("缺少 SDK venv：python3 -m venv .venv && .venv/bin/pip install openai anthropic")
from openai import OpenAI
from anthropic import Anthropic

FIXTURES = Path(__file__).resolve().parent.parent.parent / "target" / "ir_fixtures"

PASS, FAIL = [], []


def check(name: str, ok: bool, detail: str = ""):
    (PASS if ok else FAIL).append(name)
    tag = "ok  " if ok else "FAIL"
    print(f"  [{tag}] {name}" + (f" — {detail}" if detail and not ok else ""))


# ── 请求体：SDK 发出的 JSON 捕获 ────────────────────────────────────
def capture_request_schema(frames_for: "list[dict]") -> list[dict]:
    """帧列表 → 合并成单次 HTTP 响应的 bytes（SDK 流解析器消费用）。"""
    out = []
    for f in frames_for:
        if f["data"] == "[DONE]":
            out.append(b"data: [DONE]\n\n")
            continue
        parts = []
        if f.get("event"):
            parts.append(f"event: {f['event']}".encode())
        parts.append(b"data: " + f["data"].encode())
        out.append(b"\n".join(parts) + b"\n\n")
    return out


def verify_req():
    print("\n── 请求体方向：IR → 客户端格式（SDK 实际发出的 JSON） ──")
    data = json.loads((FIXTURES / "req.json").read_text())

    # Messages → anthropic SDK
    captured = {}
    def anth_handler(request: httpx.Request) -> httpx.Response:
        captured["body"] = json.loads(request.content)
        return httpx.Response(200, json={"id": "msg_mock", "type": "message", "role": "assistant", "model": "claude-opus-4-8", "content": [{"type": "text", "text": "ok"}], "stop_reason": "end_turn", "stop_sequence": None, "usage": {"input_tokens": 1, "output_tokens": 1}})

    client = Anthropic(api_key="sk-mock", http_client=httpx.Client(transport=httpx.MockTransport(anth_handler), timeout=httpx.Timeout(5.0)))
    try:
        m = client.messages.create(model="claude-opus-4-8", max_tokens=10, messages=[{"role": "user", "content": "hi"}])
        check("anthropic 请求路径可用", m.content[0].text == "ok")
    except Exception as e:
        check("anthropic 请求路径可用", False, str(e)[:120])

    # Chat Completions → openai SDK
    def chat_handler(request: httpx.Request) -> httpx.Response:
        captured["body"] = json.loads(request.content)
        return httpx.Response(200, json={"id": "chatcmpl_mock", "object": "chat.completion", "created": 1, "model": "gpt-4o", "choices": [{"index": 0, "message": {"role": "assistant", "content": "ok"}, "finish_reason": "stop"}]})

    client = OpenAI(api_key="sk-mock", base_url="http://mock", http_client=httpx.Client(transport=httpx.MockTransport(chat_handler), timeout=httpx.Timeout(5.0)))
    try:
        r = client.chat.completions.create(model="gpt-4o", messages=[{"role": "user", "content": "hi"}])
        check("chat 请求路径可用", r.choices[0].message.content == "ok")
    except Exception as e:
        check("chat 请求路径可用", False, str(e)[:120])

    # Responses → openai SDK
    def resp_handler(request: httpx.Request) -> httpx.Response:
        captured["body"] = json.loads(request.content)
        return httpx.Response(200, json={"id": "resp_mock", "object": "response", "created_at": 1, "model": "gpt-4o", "status": "completed", "output": [], "usage": {"input_tokens": 1, "output_tokens": 1, "total_tokens": 2}})

    client = OpenAI(api_key="sk-mock", base_url="http://mock", http_client=httpx.Client(transport=httpx.MockTransport(resp_handler), timeout=httpx.Timeout(5.0)))
    try:
        r = client.responses.create(model="gpt-4o", input="hi")
        check("responses 请求路径可用", r.status == "completed")
    except Exception as e:
        check("responses 请求路径可用", False, str(e)[:120])

    # 把 fixture 的三种请求体直接经 SDK 路径发出，捕获实际 body
    import importlib.util

    anth_emit_client = Anthropic(api_key="sk-mock", http_client=httpx.Client(transport=httpx.MockTransport(anth_handler), timeout=httpx.Timeout(5.0)))

    def emit(method_name: str, body: dict) -> dict | None:
        captured.clear()
        try:
            if method_name == "messages":
                anth_emit_client.messages.create(model=body.get("model", "claude-opus-4-8"), **{k: v for k, v in body.items() if k not in ("model",)})
            elif method_name == "chat_completions":
                client.chat.completions.create(model=body.get("model", "gpt-4o"), **{k: v for k, v in body.items() if k not in ("model",)})
            elif method_name == "responses":
                client.responses.create(model=body.get("model", "gpt-4o"), **{k: v for k, v in body.items() if k not in ("model",)})
            return captured.get("body")
        except Exception as e:
            check(f"{method_name} 请求体经 SDK 发出", False, str(e)[:200])
            return None

    for fmt in ("messages", "chat_completions", "responses"):
        body = data[fmt]
        sent = emit(fmt, body)
        if sent is None:
            continue
        check(f"{fmt} 请求体经 SDK 发出", True)
        # 逐字段校验：SDK 发出 = fixture（SDK 会丢弃/改写才意味着不合规）
        diffs = []
        for k, v in body.items():
            if k not in sent:
                diffs.append(f"SDK 丢弃了 {k}")
            elif sent[k] != v:
                diffs.append(f"{k}: 期望 {json.dumps(v)[:60]} 实际 {json.dumps(sent[k])[:60]}")
        check(f"{fmt} 请求体字段保真", not diffs, "; ".join(diffs[:5]))


# ── 流式方向：IR → 客户端 SSE → SDK 流解析器 ───────────────────────
def verify_stream():
    print("\n── 流式方向：IR → 客户端 SSE（SDK 流解析器消费） ──")
    data = json.loads((FIXTURES / "stream.json").read_text())

    # Messages SSE → anthropic MessageStream
    frames = data["messages"]
    raw = b"".join(
        b"event: " + f["event"].encode() + b"\ndata: " + f["data"].encode() + b"\n\n"
        for f in frames
    )
    client = Anthropic(api_key="sk-mock", http_client=httpx.Client(transport=httpx.MockTransport(lambda r: httpx.Response(200, content=raw, headers={"content-type": "text/event-stream"})), timeout=httpx.Timeout(5.0)))
    try:
        with client.messages.stream(model="claude-opus-4-8", max_tokens=10, messages=[{"role": "user", "content": "hi"}]) as stream:
            final = stream.get_final_message()
            text = "".join(b.text for b in final.content if b.type == "text")
        check("messages SSE 被 anthropic SDK 解析", text == "The weather in Tokyo is 24°C and sunny.", f"text={text!r}")
    except Exception as e:
        check("messages SSE 被 anthropic SDK 解析", False, str(e)[:200])

    # Chat SSE → openai ChatCompletionStream
    frames = data["chat_completions"]
    raw = b"".join(b"data: " + f["data"].encode() + b"\n\n" for f in frames)
    client = OpenAI(api_key="sk-mock", base_url="http://mock", http_client=httpx.Client(transport=httpx.MockTransport(lambda r: httpx.Response(200, content=raw, headers={"content-type": "text/event-stream"})), timeout=httpx.Timeout(5.0)))
    try:
        with client.chat.completions.stream(model="gpt-4o", messages=[{"role": "user", "content": "hi"}]) as stream:
            final = stream.get_final_completion()
        text = final.choices[0].message.content
        check("chat SSE 被 openai SDK 解析", text == "The weather in Tokyo is 24°C and sunny.", f"text={text!r}")
    except Exception as e:
        check("chat SSE 被 openai SDK 解析", False, str(e)[:200])

    # Responses SSE → openai ResponseStream
    frames = data["responses"]
    raw = b"".join(
        b"event: " + f["event"].encode() + b"\ndata: " + f["data"].encode() + b"\n\n"
        for f in frames
    )
    client = OpenAI(api_key="sk-mock", base_url="http://mock", http_client=httpx.Client(transport=httpx.MockTransport(lambda r: httpx.Response(200, content=raw, headers={"content-type": "text/event-stream"})), timeout=httpx.Timeout(5.0)))
    try:
        with client.responses.stream(model="gpt-4o", input="hi") as stream:
            final = stream.get_final_response()
        out_text = "".join(p.text for item in final.output if item.type == "message" for p in item.content if p.type == "output_text")
        check("responses SSE 被 openai SDK 解析", out_text == "The weather in Tokyo is 24°C and sunny.", f"text={out_text!r}")
    except Exception as e:
        check("responses SSE 被 openai SDK 解析", False, str(e)[:300])


# ── subagent 场景：thinking → text → tool → 后续 text ────────────────
def verify_subagent():
    print("\n── subagent 场景：多块生命周期（thinking → text → tool → 后续 text） ──")
    data = json.loads((FIXTURES / "subagent.json").read_text())

    frames = data["messages"]
    raw = b"".join(b"event: " + f["event"].encode() + b"\ndata: " + f["data"].encode() + b"\n\n" for f in frames)
    client = Anthropic(api_key="sk-mock", http_client=httpx.Client(transport=httpx.MockTransport(lambda r: httpx.Response(200, content=raw, headers={"content-type": "text/event-stream"})), timeout=httpx.Timeout(5.0)))
    try:
        with client.messages.stream(model="claude-opus-4-8", max_tokens=10, messages=[{"role": "user", "content": "hi"}]) as stream:
            final = stream.get_final_message()
        text = "".join(b.text for b in final.content if b.type == "text")
        tool_use = [b for b in final.content if b.type == "tool_use"]
        check("subagent messages SSE 解析（文本+工具+后续文本）", text == "Let me look at the handler.Found 3 files." and len(tool_use) == 1 and tool_use[0].name == "Bash", f"text={text!r} tool={len(tool_use)}")
    except Exception as e:
        check("subagent messages SSE 解析", False, str(e)[:200])

    frames = data["chat_completions"]
    raw = b"".join(b"data: " + f["data"].encode() + b"\n\n" for f in frames)
    client = OpenAI(api_key="sk-mock", base_url="http://mock", http_client=httpx.Client(transport=httpx.MockTransport(lambda r: httpx.Response(200, content=raw, headers={"content-type": "text/event-stream"})), timeout=httpx.Timeout(5.0)))
    try:
        with client.chat.completions.stream(model="gpt-4o", messages=[{"role": "user", "content": "hi"}]) as stream:
            final = stream.get_final_completion()
        text = final.choices[0].message.content
        tool_calls = final.choices[0].message.tool_calls or []
        args = tool_calls[0].function.arguments if tool_calls else ""
        check("subagent chat SSE 解析", text == "Let me look at the handler.Found 3 files." and "ls" in args, f"text={text!r} args={args!r}")
    except Exception as e:
        check("subagent chat SSE 解析", False, str(e)[:200])

    frames = data["responses"]
    raw = b"".join(b"event: " + f["event"].encode() + b"\ndata: " + f["data"].encode() + b"\n\n" for f in frames)
    client = OpenAI(api_key="sk-mock", base_url="http://mock", http_client=httpx.Client(transport=httpx.MockTransport(lambda r: httpx.Response(200, content=raw, headers={"content-type": "text/event-stream"})), timeout=httpx.Timeout(5.0)))
    try:
        with client.responses.stream(model="gpt-4o", input="hi") as stream:
            final = stream.get_final_response()
        out_text = "".join(p.text for item in final.output if item.type == "message" for p in item.content if p.type == "output_text")
        calls = [i for i in final.output if i.type == "function_call"]
        check("subagent responses SSE 解析", out_text == "Let me look at the handler.Found 3 files." and len(calls) == 1 and "ls" in calls[0].arguments, f"text={out_text!r} calls={len(calls)}")
    except Exception as e:
        check("subagent responses SSE 解析", False, str(e)[:300])


# ── 解析方向：上游 chunk → IR 事件（结构化断言） ───────────────────
def verify_parse():
    print("\n── 解析方向：上游格式 chunk → IR 事件 ──")
    data = json.loads((FIXTURES / "parse.json").read_text())

    ev = data["anthropic_events"]
    kinds = [e["type"] for e in ev]
    check("anthropic 解析事件序列", kinds == ["MessageStart", "ContentBlockStart", "ContentBlockDelta", "ContentBlockStop", "ContentBlockStart", "ContentBlockDelta", "ContentBlockStop", "MessageDelta", "MessageStop"], f"{kinds}")
    thinking = [e for e in ev if e["type"] == "ContentBlockDelta" and e["delta"]["kind"] == "Thinking"]
    check("anthropic thinking 内容保留", thinking and thinking[0]["delta"]["text"] == "Thinking...", str(thinking)[:100])
    u = data["anthropic_usage"]
    check("anthropic usage 提取", u["input_tokens"] == 210 and u["cache_read_input_tokens"] == 8000 and u["cache_creation_input_tokens"] == 50 and u["output_tokens"] == 30, str(u))

    ev = data["chat_events"]
    kinds = [e["type"] for e in ev]
    text = "".join(e["delta"]["text"] for e in ev if e["type"] == "ContentBlockDelta" and e["delta"]["kind"] == "Text")
    check("chat 解析文本拼接", text == "Hello from chat upstream", f"text={text!r}")
    md = [e for e in ev if e["type"] == "MessageDelta"]
    check("chat stop_reason=EndTurn", md and md[0]["stop_reason"] == "end_turn", str(md[:1])[:100])
    u = data["chat_usage"]
    check("chat usage 提取", u["input_tokens"] == 200 and u["output_tokens"] == 45 and u["cache_read_input_tokens"] == 700, str(u))

    ev = data["responses_events"]
    kinds = [e["type"] for e in ev]
    text = "".join(e["delta"]["text"] for e in ev if e["type"] == "ContentBlockDelta" and e["delta"]["kind"] == "Text")
    check("responses 解析文本拼接", text == "Hello from responses", f"text={text!r}")
    thinking = [e for e in ev if e["type"] == "ContentBlockDelta" and e["delta"]["kind"] == "Thinking"]
    check("responses thinking 内容保留", thinking and thinking[0]["delta"]["text"] == "Deep thinking", str(thinking)[:100])
    u = data["responses_usage"]
    check("responses usage 提取", u["input_tokens"] == 500 and u["output_tokens"] == 25 and u["cache_read_input_tokens"] == 300, str(u))


# ── WebSearch 场景：enriched IR（已注入搜索结果）→ SDK 消费验证 ────────────
def verify_websearch():
    print("\n── WebSearch 场景：enriched IR（已注入搜索结果） ──")
    data = json.loads((FIXTURES / "websearch_req.json").read_text())

    # ── 请求体结构验证 ──
    msg_body = data["messages"]
    msg_system = msg_body.get("system")
    check("messages system 是数组", isinstance(msg_system, list), f"type={type(msg_system).__name__}")
    if isinstance(msg_system, list):
        check("messages system 有 2 个 block", len(msg_system) == 2, f"len={len(msg_system)}")
        check("messages system[0] 是原始 prompt", msg_system[0].get("text") == "You are a helpful assistant.", str(msg_system[0])[:80])
        check("messages system[1] 含搜索结果", "[Web Search Results" in msg_system[1].get("text", ""), str(msg_system[1])[:80])
    check("messages tools 字段缺失", "tools" not in msg_body, f"tools={msg_body.get('tools')}")
    check("messages tool_choice 字段缺失", "tool_choice" not in msg_body, f"tool_choice={msg_body.get('tool_choice')}")

    chat_body = data["chat_completions"]
    chat_msgs = chat_body.get("messages", [])
    check("chat messages[0] 是 system", chat_msgs and chat_msgs[0].get("role") == "system", str(chat_msgs[:1])[:80])
    if chat_msgs:
        sys_content = chat_msgs[0].get("content", "")
        check("chat system 含原始 prompt", "You are a helpful assistant." in sys_content, sys_content[:80])
        check("chat system 含搜索结果", "[Web Search Results" in sys_content, sys_content[:80])
    check("chat tools 字段缺失", "tools" not in chat_body, f"tools={chat_body.get('tools')}")
    check("chat tool_choice 字段缺失", "tool_choice" not in chat_body, f"tool_choice={chat_body.get('tool_choice')}")

    resp_body = data["responses"]
    resp_input = resp_body.get("input", [])
    check("responses input[0] 是 system", resp_input and resp_input[0].get("role") == "system", str(resp_input[:1])[:80])
    if resp_input:
        sys_content = resp_input[0].get("content", [])
        check("responses system content 是数组", isinstance(sys_content, list), f"type={type(sys_content).__name__}")
        if isinstance(sys_content, list):
            check("responses system 有 2 个 part", len(sys_content) == 2, f"len={len(sys_content)}")
            check("responses system[0] 是 input_text", sys_content[0].get("type") == "input_text", str(sys_content[0])[:80])
            check("responses system[1] 含搜索结果", "[Web Search Results" in sys_content[1].get("text", ""), str(sys_content[1])[:80])
    check("responses tools 字段缺失", "tools" not in resp_body, f"tools={resp_body.get('tools')}")
    check("responses tool_choice 字段缺失", "tool_choice" not in resp_body, f"tool_choice={resp_body.get('tool_choice')}")

    # ── SDK 实际发送验证（MockTransport 捕获 SDK 发出的请求体） ──
    captured = {}

    def anth_handler(request: httpx.Request) -> httpx.Response:
        captured["body"] = json.loads(request.content)
        return httpx.Response(200, json={"id": "msg_mock", "type": "message", "role": "assistant", "model": "claude-sonnet-4-20250514", "content": [{"type": "text", "text": "ok"}], "stop_reason": "end_turn", "stop_sequence": None, "usage": {"input_tokens": 1, "output_tokens": 1}})

    anth_client = Anthropic(api_key="sk-mock", http_client=httpx.Client(transport=httpx.MockTransport(anth_handler), timeout=httpx.Timeout(5.0)))
    try:
        captured.clear()
        anth_client.messages.create(
            model=msg_body.get("model", "claude-sonnet-4-20250514"),
            max_tokens=msg_body.get("max_tokens", 10),
            **{k: v for k, v in msg_body.items() if k not in ("model", "max_tokens")}
        )
        sent = captured.get("body", {})
        check("messages 请求体经 SDK 发出", bool(sent), str(sent)[:100])
        sent_system = sent.get("system")
        check("SDK 保留 system 数组", isinstance(sent_system, list) and len(sent_system) == 2, f"sent_system={str(sent_system)[:80]}")
        check("SDK 不注入 tools", "tools" not in sent, f"tools={sent.get('tools')}")
    except Exception as e:
        check("messages 请求体经 SDK 发出", False, str(e)[:200])

    def chat_handler(request: httpx.Request) -> httpx.Response:
        captured["body"] = json.loads(request.content)
        return httpx.Response(200, json={"id": "chatcmpl_mock", "object": "chat.completion", "created": 1, "model": "gpt-4o", "choices": [{"index": 0, "message": {"role": "assistant", "content": "ok"}, "finish_reason": "stop"}]})

    openai_client = OpenAI(api_key="sk-mock", base_url="http://mock", http_client=httpx.Client(transport=httpx.MockTransport(chat_handler), timeout=httpx.Timeout(5.0)))
    try:
        captured.clear()
        openai_client.chat.completions.create(
            model=chat_body.get("model", "gpt-4o"),
            **{k: v for k, v in chat_body.items() if k not in ("model",)}
        )
        sent = captured.get("body", {})
        check("chat 请求体经 SDK 发出", bool(sent), str(sent)[:100])
        check("chat SDK 不注入 tools", "tools" not in sent, f"tools={sent.get('tools')}")
    except Exception as e:
        check("chat 请求体经 SDK 发出", False, str(e)[:200])

    try:
        captured.clear()
        openai_client.responses.create(
            model=resp_body.get("model", "gpt-4o"),
            **{k: v for k, v in resp_body.items() if k not in ("model",)}
        )
        sent = captured.get("body", {})
        check("responses 请求体经 SDK 发出", bool(sent), str(sent)[:100])
        check("responses SDK 不注入 tools", "tools" not in sent, f"tools={sent.get('tools')}")
    except Exception as e:
        check("responses 请求体经 SDK 发出", False, str(e)[:200])


def main():
    data = json.loads((FIXTURES / "req.json").read_text())
    if not data:
        print("fixtures 缺失：先运行 cargo test --lib sdk_fixtures")
        sys.exit(2)
    verify_req()
    verify_stream()
    verify_parse()
    verify_subagent()
    verify_websearch()
    print(f"\n── 结果：{len(PASS)} 通过 / {len(FAIL)} 失败 ──")
    if FAIL:
        print("失败项：")
        for f in FAIL:
            print(f"  ✗ {f}")
        sys.exit(1)
    print("全部通过")


if __name__ == "__main__":
    main()

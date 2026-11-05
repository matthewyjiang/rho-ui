"""A scripted ACP v2 agent for tests. Speaks JSON-RPC lines on stdio.

rho-ui-core's agent tests and rho-ui's headless UI tests both run it.

- session/new returns "new-1", "new-2", ...
- session/list returns two pages: "s1" (titled "first") and "old" (untitled).
- session/resume replays one agent message, "replayed".
- Settings: mode (select), model (select), fast_mode (toggle).
- A prompt asks the host for permission, then runs a fake `ls` tool and
  replies "you chose <option id>". The prompt "fail" is refused.
"""
import json
import sys

next_id = 1000
new_sessions = 0
pending = {}  # our request id -> session id waiting on it
settings = {}  # session id -> {config id: value}

MODELS = [("fake/small", "Small"), ("fake/large", "Large"), ("fake/huge", "Huge")]


def send(message):
    sys.stdout.write(json.dumps({"jsonrpc": "2.0", **message}) + "\n")
    sys.stdout.flush()


def update(session, body):
    send({"method": "session/update", "params": {"sessionId": session, "update": body}})


def config(session):
    values = settings.setdefault(session, {"mode": "auto", "model": "fake/small", "fast_mode": False})

    def select(config_id, name, choices):
        return {"configId": config_id, "name": name, "type": "select",
                "currentValue": values[config_id],
                "options": [{"value": value, "name": label} for value, label in choices]}

    return [
        select("mode", "Mode", [("auto", "Auto"), ("plan", "Plan")]),
        select("model", "Model", MODELS),
        {"configId": "fast_mode", "name": "Fast", "type": "boolean",
         "currentValue": values["fast_mode"]},
    ]


def chunk(session, message_id, text):
    update(session, {"sessionUpdate": "agent_message_chunk", "messageId": message_id,
                     "content": {"type": "text", "text": text}})


def finish_turn(session, chosen):
    if chosen != "cancelled":
        update(session, {"sessionUpdate": "tool_call_update", "toolCallId": "tool-1", "title": "ls",
                         "kind": "execute", "status": "completed",
                         "content": [{"type": "content",
                                      "content": {"type": "text", "text": "a.rs\nb.rs"}}]})
    chunk(session, "m2", f"you chose {chosen}")
    update(session, {"sessionUpdate": "usage_update", "used": 2500, "size": 10000,
                     "cost": {"amount": 0.01, "currency": "USD"}})
    update(session, {"sessionUpdate": "state_update", "state": "idle", "stopReason": "end_turn"})


for line in sys.stdin:
    msg = json.loads(line)
    method, rid, params = msg.get("method"), msg.get("id"), msg.get("params", {})

    if method is None:  # a response to our permission request
        session = pending.pop(rid)
        finish_turn(session, msg["result"]["outcome"].get("optionId", "cancelled"))
    elif method == "initialize":
        send({"id": rid, "result": {"protocolVersion": 2, "info": {"name": "fake", "version": "9.9.9"},
                                    "capabilities": {"session": {}}}})
    elif method == "session/new":
        new_sessions += 1
        session = f"new-{new_sessions}"
        send({"id": rid, "result": {"sessionId": session, "configOptions": config(session),
                                    "availableCommands": [{"name": "compact", "description": "Compact"}]}})
    elif method == "session/list":
        # Two pages, to exercise the cursor.
        if params.get("cursor") is None:
            send({"id": rid, "result": {"sessions": [{"sessionId": "s1", "cwd": "/w/a", "title": "first",
                                                      "updatedAt": "2026-01-02T00:00:00Z"}],
                                        "nextCursor": "p2"}})
        else:
            send({"id": rid, "result": {"sessions": [{"sessionId": "old", "cwd": "/w/b",
                                                      "updatedAt": "2026-01-01T00:00:00Z"}]}})
    elif method == "session/resume":
        assert params["replayFrom"] == {"type": "start"}, params
        chunk(params["sessionId"], "m0", "replayed")
        update(params["sessionId"], {"sessionUpdate": "state_update", "state": "idle"})
        send({"id": rid, "result": {"configOptions": config(params["sessionId"]), "availableCommands": []}})
    elif method == "session/set_config_option":
        session = params["sessionId"]
        config(session)
        settings[session][params["configId"]] = params["value"]
        send({"id": rid, "result": {"configOptions": config(session)}})
    elif method == "session/prompt":
        session = params["sessionId"]
        if params["prompt"][0]["text"] == "fail":
            send({"id": rid, "error": {"code": -32602, "message": "Invalid params", "data": "busy"}})
            continue
        update(session, {"sessionUpdate": "user_message", "messageId": "m1", "content": params["prompt"]})
        update(session, {"sessionUpdate": "state_update", "state": "running"})
        send({"id": rid, "result": {"messageId": "m1"}})
        next_id += 1
        pending[next_id] = session
        send({"id": next_id, "method": "session/request_permission",
              "params": {"sessionId": session, "title": "Run `ls`?",
                         "options": [{"optionId": "yes", "name": "Allow once", "kind": "allow_once"},
                                     {"optionId": "no", "name": "Reject", "kind": "reject_once"}]}})
    elif rid is not None:
        send({"id": rid, "result": {}})

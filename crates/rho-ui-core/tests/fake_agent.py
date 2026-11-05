"""A scripted ACP v2 agent for rho-ui-core's tests. Speaks JSON-RPC lines on stdio.

Sessions: "s1" is created by session/new; "old" exists for list/resume.
A prompt asks the host for permission, then echoes the chosen option id.
"""
import json
import sys

next_id = 1000
pending = {}  # our request id -> session id waiting on it


def send(message):
    sys.stdout.write(json.dumps({"jsonrpc": "2.0", **message}) + "\n")
    sys.stdout.flush()


def update(session, body):
    send({"method": "session/update", "params": {"sessionId": session, "update": body}})


def config(mode):
    return [{"configId": "mode", "name": "Mode", "type": "select", "currentValue": mode,
             "options": [{"value": "auto", "name": "Auto"}, {"value": "plan", "name": "Plan"}]}]


def chunk(session, message_id, text):
    update(session, {"sessionUpdate": "agent_message_chunk", "messageId": message_id,
                     "content": {"type": "text", "text": text}})


for line in sys.stdin:
    msg = json.loads(line)
    method, rid, params = msg.get("method"), msg.get("id"), msg.get("params", {})

    if method is None:  # a response to our permission request
        session = pending.pop(rid)
        outcome = msg["result"]["outcome"]
        chosen = outcome.get("optionId", "cancelled")
        chunk(session, "m2", f"you chose {chosen}")
        update(session, {"sessionUpdate": "state_update", "state": "idle", "stopReason": "end_turn"})
    elif method == "initialize":
        send({"id": rid, "result": {"protocolVersion": 2, "info": {"name": "fake", "version": "9.9.9"},
                                    "capabilities": {"session": {}}}})
    elif method == "session/new":
        send({"id": rid, "result": {"sessionId": "s1", "configOptions": config("auto"),
                                    "availableCommands": [{"name": "compact", "description": "Compact"}]}})
    elif method == "session/list":
        # Two pages, to exercise the cursor.
        if params.get("cursor") is None:
            send({"id": rid, "result": {"sessions": [{"sessionId": "s1", "cwd": "/w/a", "title": "first"}],
                                        "nextCursor": "p2"}})
        else:
            send({"id": rid, "result": {"sessions": [{"sessionId": "old", "cwd": "/w/b"}]}})
    elif method == "session/resume":
        assert params["replayFrom"] == {"type": "start"}, params
        chunk(params["sessionId"], "m0", "replayed")
        update(params["sessionId"], {"sessionUpdate": "state_update", "state": "idle"})
        send({"id": rid, "result": {"configOptions": config("auto"), "availableCommands": []}})
    elif method == "session/set_config_option":
        send({"id": rid, "result": {"configOptions": config(params["value"])}})
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

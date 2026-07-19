import json
import os
import threading

from nanocodex import Nanocodex


agent, events = Nanocodex(os.environ["OPENAI_API_KEY"])


def print_events() -> None:
    while encoded := events.recv_json():
        event = json.loads(encoded)
        print(json.dumps(event, separators=(",", ":")), flush=True)


threading.Thread(target=print_events, daemon=True).start()
turn = agent.prompt("Inspect this repository and summarize it.")
print(turn.result())

"""Stream the ordered event feed on a Python thread while awaiting a turn."""

import json
import os
import threading

from nanocodex import Nanocodex


agent, events = Nanocodex(os.environ["OPENAI_API_KEY"], thinking="low")


def print_events() -> None:
    while (line := events.recv_json()) is not None:
        event = json.loads(line)
        print(f"event {event['seq']}: {event['type']}")


threading.Thread(target=print_events, daemon=True).start()
turn = agent.prompt("Reply with exactly PYTHON_OK and nothing else.")
print(turn.result())

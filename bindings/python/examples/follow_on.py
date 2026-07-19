"""Persistent follow-on prompting through the embedded PyO3 binding."""

import os

from nanocodex import Nanocodex


agent, _ = Nanocodex(os.environ["OPENAI_API_KEY"], thinking="low")

first = agent.prompt("Choose one short word for this project.")
print("first:", first.result())

# The Rust-owned session already retains the first turn. Nothing is passed back.
second = agent.prompt("Return that same word in uppercase.")
print("second:", second.result())

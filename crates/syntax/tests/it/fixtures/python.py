"""Module docstring."""
import os
from typing import Optional

MAX_SIZE = 64


@decorator
class Config(Base):
    def __init__(self, path: str, size: int = 8, *args, **kwargs):
        self.path = path  # trailing comment
        self.size = size

    def load(self) -> Optional[dict]:
        if not os.path.exists(self.path):
            return None
        with open(self.path) as handle:
            data = {"key": [1, 2.5, True, None]}
        return len(data) + MAX_SIZE


def helper(x, y=f"{1 + 2}"):
    return lambda z: x if y else z

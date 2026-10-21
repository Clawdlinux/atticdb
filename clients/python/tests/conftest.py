import os
import sys

import pytest

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))

from helpers import Server  # noqa: E402


@pytest.fixture
def server():
    server = Server()
    yield server
    server.close()

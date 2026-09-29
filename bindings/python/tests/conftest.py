import pytest

import bkndb


@pytest.fixture()
def db():
    """A fresh in-memory database, closed automatically after the test."""
    with bkndb.in_memory() as database:
        yield database

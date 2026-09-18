"""Pre-send validation of idle HTTP connections; never replay a sent request."""
import select


def reusable_idle_connection(connection):
    """The previous response was fully consumed; pipelining is not used.

    An idle socket readable before sending the next request may contain peer
    shutdown or unsolicited TLS data. Discard it before sending anything new.
    A close racing the subsequent send remains a recorded request failure.
    """
    if connection is None or connection.sock is None:
        return connection
    try:
        readable, _, errors = select.select(
            [connection.sock], [], [connection.sock], 0)
        if not readable and not errors:
            return connection
    except (OSError, ValueError):
        pass
    connection.close()
    return None

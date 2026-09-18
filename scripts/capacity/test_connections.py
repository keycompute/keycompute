import http.client
import socket
from types import SimpleNamespace
import unittest
from unittest.mock import Mock, patch
import workload
from connections import reusable_idle_connection


class ConnectionTests(unittest.TestCase):
    def test_closed_idle_socket_is_discarded_before_new_request(self):
        local, peer = socket.socketpair()
        conn = SimpleNamespace(sock=local, close=Mock(side_effect=local.close))
        try:
            self.assertIs(reusable_idle_connection(conn), conn)
            peer.close()
            self.assertIsNone(reusable_idle_connection(conn))
            conn.close.assert_called_once()
        finally:
            local.close()
            peer.close()

    def test_unsent_connection_is_still_connectable(self):
        conn = SimpleNamespace(sock=None)
        self.assertIs(reusable_idle_connection(conn), conn)
        self.assertIsNone(reusable_idle_connection(None))

    def test_missing_response_does_not_retry_sent_post(self):
        conn = Mock()
        conn.getresponse.side_effect = http.client.RemoteDisconnected()
        previous = getattr(workload.thread_state, 'connection', None)
        workload.thread_state.connection = conn
        try:
            with patch('workload.reusable_idle_connection', return_value=conn):
                sample = workload.request('disposable-key', b'{}', False, 0, 1)
            self.assertFalse(sample['complete'])
            self.assertEqual(sample['status'], 0)
            self.assertEqual(sample['transport_error'], 'headers:RemoteDisconnected')
            conn.request.assert_called_once()
            conn.close.assert_called_once()
        finally:
            workload.thread_state.connection = previous


if __name__ == '__main__':
    unittest.main()

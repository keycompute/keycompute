import copy
import unittest
from plan import derive,assess

class PlanTests(unittest.TestCase):
    def test_five_second_stream_does_not_hide_tenant_bottleneck(self):
        result=derive(rate=40,stream_ms=5000)
        self.assertEqual(result['required_with_headroom'],{'cluster_inflight':252,'instance_inflight':252,
            'hot_tenant_inflight_on_one_instance':63,'shared_account_inflight':32})
        self.assertEqual(result['gateway_admission'],{'global_limit':256,'tenant_limit':63,'account_limit':32,
            'global_queue':20,'tenant_queue':5,'queue_timeout_ms':500})
    def test_no_automatic_global_increase_or_writer_inference(self):
        result=derive(rate=100,stream_ms=5000,writer_connections=10)
        self.assertFalse(result['feasible_by_arithmetic']);self.assertIsNone(result['gateway_admission'])
        self.assertEqual(result['writer_connections_trial'],10)
    def test_shared_account_limit_is_not_divided_by_replicas(self):
        a=derive(rate=40,stream_ms=5000,replicas=2)
        self.assertEqual(a['required_with_headroom']['shared_account_inflight'],32)
        self.assertEqual(a['required_with_headroom']['instance_inflight'],126)
        self.assertEqual(a['required_with_headroom']['hot_tenant_inflight_on_one_instance'],63)
        b=derive(rate=40,stream_ms=5000,replicas=2,hottest_instance_percent=100,hottest_tenant_percent=70)
        self.assertEqual(b['required_with_headroom']['instance_inflight'],252)
        self.assertEqual(b['required_with_headroom']['hot_tenant_inflight_on_one_instance'],177)
    def test_invalid_or_unsafe_assumptions_fail(self):
        for invalid in [{'rate':0},{'rate':True},{'stream_ms':60001},{'queue_ms':0},{'writer_connections':65},
                        {'replicas':2,'hottest_instance_percent':49},{'hottest_tenant_percent':10}]:
            with self.subTest(invalid=invalid),self.assertRaises(ValueError):derive(**({'rate':40,'stream_ms':5000}|invalid))
    def test_success_without_latency_or_funds_is_not_pass(self):
        good={'planned':100,'completed_success':100,'generator_dropped':0,
              'first_content_ms':{'p99':550},'complete_ms':{'p99':5100}}
        self.assertTrue(assess(good,{'pass':True},first_content_p99_ms=1000,complete_p99_ms=6000)['accepted'])
        for change in [{'generator_dropped':1},{'completed_success':98},{'first_content_ms':None},
                       {'complete_ms':{'p99':7000}},{'complete_ms':{'p99':float('nan')}}]:
            self.assertFalse(assess(good|change,{'pass':True},first_content_p99_ms=1000,complete_p99_ms=6000)['accepted'])
        self.assertFalse(assess(good,{'pass':False},first_content_p99_ms=1000,complete_p99_ms=6000)['accepted'])
        with self.assertRaises(ValueError):assess(good,{'pass':True},first_content_p99_ms=float('nan'),complete_p99_ms=6000)

if __name__=='__main__':unittest.main()

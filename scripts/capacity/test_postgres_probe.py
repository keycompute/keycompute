import copy
import unittest
from postgres_probe import check_owner, classify, difference

class ProbeTest(unittest.TestCase):
    def fixture(self):
        return {'database':'kc_load_test','wal':{'stats_reset':'one','wal_bytes':100},
                'statements_info':{'stats_reset':'one','dealloc':0}, 'io_settings':{},
                'statements':[{'queryid':'1','toplevel':True,'class':'select:usage_logs','calls':10,'total_exec_time':5.0}]}
    def test_finite_interval_and_missing_calls(self):
        before=self.fixture();after=copy.deepcopy(before)
        after['statements'][0]['calls']+=3;after['wal']['wal_bytes']+=20
        out=difference(before,after)
        self.assertTrue(out['valid_interval']);self.assertEqual(out['wal_delta']['wal_bytes'],20)
        self.assertEqual(out['statements_delta'][0]['calls'],3)
    def test_resets_and_eviction_never_look_like_valid_speedup(self):
        for mode in ['reset','evicted','missing','decreased']:
            before=self.fixture();after=copy.deepcopy(before)
            if mode=='reset':after['wal']['stats_reset']='two'
            if mode=='evicted':after['statements_info']['dealloc']=1
            if mode=='missing':after['statements']=[]
            if mode=='decreased':after['statements'][0]['calls']=0
            self.assertFalse(difference(before,after)['valid_interval'])
    def test_same_query_by_different_roles_is_not_collapsed(self):
        before=self.fixture();before['statements'][0]['userid']='1'
        before['statements'].append(dict(before['statements'][0],userid='2',calls=20))
        after=copy.deepcopy(before);after['statements'][0]['calls']+=2;after['statements'][1]['calls']+=5
        out=difference(before,after)
        self.assertEqual({s['userid']:s['calls'] for s in out['statements_delta']},{'1':2,'2':5})
    def test_query_text_and_wrong_owners_are_not_accepted(self):
        secret="SELECT * FROM usage_logs WHERE secret='private-token'"
        self.assertEqual(classify(secret),'select:usage_logs')
        self.assertNotIn('private',classify(secret))
        with self.assertRaises(ValueError):check_owner({'Config':{'Labels':{}}},'a'*12)
        check_owner({'Config':{'Labels':{'keycompute.capacity_run':'a'*12}}},'a'*12)

if __name__=='__main__':unittest.main()

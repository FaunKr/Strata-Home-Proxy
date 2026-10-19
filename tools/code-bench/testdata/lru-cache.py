from collections import OrderedDict
class LRUCache:
    def __init__(self,capacity):
        if capacity<1: raise ValueError(capacity)
        self._c=capacity; self._d=OrderedDict(); self._e=0
    @property
    def evictions(self): return self._e
    def get(self,key,default=None):
        if key not in self._d: return default
        self._d.move_to_end(key); return self._d[key]
    def put(self,key,value):
        self._d[key]=value; self._d.move_to_end(key); self._trim()
    def _trim(self):
        while len(self._d)>self._c: self._d.popitem(last=False); self._e+=1
    def resize(self,capacity):
        if capacity<1: raise ValueError(capacity)
        self._c=capacity; self._trim()
    def __len__(self): return len(self._d)
    def __contains__(self,k): return k in self._d
    def keys(self): return list(self._d)

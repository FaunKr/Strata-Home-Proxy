V=[(1000,"M"),(900,"CM"),(500,"D"),(400,"CD"),(100,"C"),(90,"XC"),(50,"L"),(40,"XL"),(10,"X"),(9,"IX"),(5,"V"),(4,"IV"),(1,"I")]
def to_roman(n):
    if type(n) is not int or not 1<=n<=3999: raise ValueError(n)
    out=""
    for v,s in V:
        while n>=v: out+=s; n-=v
    return out
_T={to_roman(i):i for i in range(1,4000)}
def from_roman(s):
    if not isinstance(s,str): raise ValueError(s)
    try: return _T[s.upper()]
    except KeyError: raise ValueError(s)

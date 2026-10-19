import csv
from decimal import Decimal, InvalidOperation
def summarize(path):
    out={}; names={}
    with open(path,newline="") as f:
        for row in csv.DictReader(f):
            region=(row.get("region") or "").strip()
            try:
                q=int(row["quantity"]); p=float(row["unit_price"])
            except (ValueError,TypeError,KeyError): continue
            if q<1 or p<0 or not region or p!=p: continue
            key=region.lower(); names.setdefault(key,region)
            r=out.setdefault(key,{"orders":0,"revenue":0.0,"products":{}})
            r["orders"]+=1; r["revenue"]+=q*p
            r["products"][row["product"]]=r["products"].get(row["product"],0)+q*p
    res={}
    for k,r in out.items():
        top=sorted(r["products"].items(),key=lambda kv:(-kv[1],kv[0]))[0][0]
        res[names[k]]={"orders":r["orders"],"revenue":round(r["revenue"],2),"top_product":top}
    return res

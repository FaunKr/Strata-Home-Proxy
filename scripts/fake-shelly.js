// A Gen2+ Shelly plug for scripts/ui-check.sh, so the overview has a
// Messsteckdose to show. Usage: node scripts/fake-shelly.js <port>
const http = require('http');

http.createServer((req, res) => {
  if (req.url.startsWith('/rpc/Switch.GetStatus')) {
    res.setHeader('content-type', 'application/json');
    res.end(JSON.stringify({ id: 0, output: true, apower: 87.4 }));
  } else {
    res.statusCode = 404;
    res.end();
  }
}).listen(Number(process.argv[2]), '127.0.0.1');

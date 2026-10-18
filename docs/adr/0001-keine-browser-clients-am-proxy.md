# Keine Browser-Clients an der Proxy-API

Die Clients des Proxys sind Programme wie Claude Code und Open WebUIs Server,
keine Webseiten im Browser. Darum gibt es kein CORS: auch ein Preflight
(`OPTIONS`) ohne gültigen API-Key bekommt `401` und weckt den PC nicht. Die
Weboberfläche unter `/admin` liegt auf demselben Ursprung und braucht kein
CORS.

Wer das im Review als Fehler meldet: entschieden. Ändern nur, wenn ein Client
dazukommt, der wirklich im Browser läuft.

---
name: steward
description: PR in diesem Repo zum Merge bringen. Lesen beim Öffnen eines PRs, bei jedem CI- oder Review-Ereignis und vor jedem Merge.
---

1. **Mergebar zuerst.** Vor dem Warten auf CI und vor dem Merge `mergeable_state` des PRs lesen. Bei `dirty` `origin/main` per Merge-Commit einmischen, nie per Rebase, und `scripts/check.sh` ausführen. Danach pushen und erst dann auf CI warten. Ein CI-Lauf auf einem Kopf mit Konflikt zählt nicht.
2. **Grün heißt:** der Check `ci-ok` ist erfolgreich auf dem aktuellen Kopf (er fasst `test`, `image` und `ui` zusammen; nicht nötige Jobs werden übersprungen), und `mergeable_state` ist `clean`.
3. **Mergen** mit Merge-Commit (`merge_method: merge`), wie bei allen bisherigen PRs, und mit `expectedHeadSha` auf dem geprüften Kopf.
4. **Review-Anfrage:** Bei PRs, die Faun angestoßen hat, ist Faun der Autor. Eine Review-Anfrage an Faun scheitert deshalb. Die Zuweisung reicht.

# Spec Delta

## REMOVED Requirements

### Requirement: 生成した HFB は書き込みの許可リストに含めない
**Reason**: `qmk-flash` で、生成した `default` と `via` の HFB を書き込みの対象にする。
**Migration**: 書き込みの許可は `firmware-update` の許可リストで定める。許可するのは SHA-256 で固定した 2 つだけで、他の HFB は引き続き拒否する。

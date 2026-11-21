# 构建怎么跑

> 2026-11-21 起。负责人要求按 `docs/PLAN.md` 完成整个项目：规划和审查交给 Claude Fable 5.2，开发交给 Claude Opus 5.5。

## 角色

| 角色 | 模型 | 做什么 | 写在哪里 |
|---|---|---|---|
| 规划 | Fable 5.2 | 把 `PLAN.md` §8 的一期拆成按顺序做的任务，并定下任务之间要一致的设计 | `phases/Pn.md`、`tasks/Pn-NN-*.md` |
| 开发 | Opus 5.5 | 一次做一件任务：先写测试，再改代码，过门禁，写报告，提交 | 代码、`reports/Pn-NN-*.md` |
| 审查 | Fable 5.2 | 一期做完后审一遍；要改的写成修复任务，修完再审，最多三轮 | `reviews/Pn-rK.md`、`tasks/Pn-RK-NN-*.md` |
| 驱动 | 脚本 `scripts/build/run.sh` | 按顺序叫各个代理，每件任务都过 `scripts/build/check.sh`；过不去就停下来，说明原因 | 本机 `~/Zuzalu/zutalk-build/` |

## 门禁

`scripts/build/check.sh` 依次检查：rustfmt、`clippy -D warnings`、`scripts/build/rules/` 下的规则脚本、`cargo test --workspace`、界面的 `npm run check` 和 `npm test`。

- 每件任务都必须过门禁才算完。
- 没过的话，开发代理拿着门禁的输出最多再修三次；还不过，构建就停下。
- `PLAN.md` 里能用机器检查的规则（例如没有无上限的队列），由让代码遵守它的那件任务加成规则脚本。规则脚本只能加不能减，减要负责人同意。

## 文件

```text
docs/build/
  README.md        本文
  DECISIONS.md     负责人未决问题的临时答案（负责人可以改）
  phases/Pn.md     一期的计划；Pn.base 记这一期从哪个提交开始；Pn.done 表示审查已通过
  tasks/           任务说明，按文件名排序执行
  reports/         开发报告，第一行是「状态：完成」或「状态：受阻」
  reviews/         审查结论，第一行是「结论：通过」或「结论：要改」
~/Zuzalu/zutalk-build/   （不在仓库里）
  STATUS.md        进度流水
  logs/            每个代理的输出、每次门禁的输出
  sessions/        每个代理的完整会话，可以用 pi --session 打开看
```

## 怎么用

```bash
caffeinate -is scripts/build/run.sh               # 从停下的地方接着跑完所有阶段
PHASES="P2" scripts/build/run.sh                  # 只跑某一期
touch docs/build/HOLD                             # 下一个代理开始前暂停；删掉就继续
tail -f ~/Zuzalu/zutalk-build/STATUS.md           # 看进度
```

已经完成的任务（报告第一行是「状态：完成」）和已经通过的阶段不会重做。

## 提交

每件任务、每份计划、每轮审查各提交一次，提交在本机 `main` 上。不推送，不打标签，不发布。

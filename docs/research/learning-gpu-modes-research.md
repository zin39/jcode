# Research: Learning Mode, GPU Infra/Ops Mode, and Retention Store

## 1. LEARNING MODE

### DO
- **Start with Socratic prompts and hints before solutions.** Rationale: guided questioning forces learners to articulate reasoning instead of outsourcing the work. Source: https://arxiv.org/html/2409.05511v1
- **Escalate from question → hint → worked example → direct answer.** Rationale: novices need scaffolding, while direct answers should be a fallback to avoid cognitive overload. Source: https://research.mental-momentum.ai/r/socratic-ai-tutoring-conceptual-mdgdl9
- **Ask the learner to generate a step, prediction, explanation, or test before revealing answers.** Rationale: generation and retrieval improve durable learning more than passive exposure. Source: https://www.science.org/cms/asset/882cef39-74f8-4177-a127-dd7191b24d4e/pap.pdf
- **Build retrieval practice into the chat loop.** Rationale: active recall/testing strengthens long-term memory and comprehension compared with rereading. Source: https://pmc.ncbi.nlm.nih.gov/articles/PMC12292765/
- **Schedule spaced follow-ups from prior conversations.** Rationale: distributed retrieval is among the highest-utility learning techniques for retention. Source: https://www.supermemo.com/en/blog/twenty-rules-of-formulating-knowledge
- **Check understanding before moving on.** Rationale: ChatGPT Study Mode-style tutoring uses comprehension checks and learner background to reduce answer copying. Source: https://lennartnacke.substack.com/p/11-chatgpt-study-mode-the-0-tutor
- **Use curriculum/task context when available.** Rationale: Khanmigo’s value is not just GPT-4, but Socratic prompts plus lesson context and guardrails. Source: https://www.freethink.com/consumer-tech/khanmigo-ai-tutor
- **Keep explanations short and adapt depth to prior knowledge.** Rationale: cognitive-load theory warns that excessive detail can overwhelm working memory. Source: https://www.structural-learning.com/post/cognitive-load-theory-a-teachers-guide
- **Make the learner compare their answer to the model answer.** Rationale: contrastive feedback turns mistakes into retrieval cues and reduces false confidence. Source: https://www.retrievalpractice.org/strategies/2018/5/11/retrieve-taking
- **Create end-of-session review artifacts: key ideas, open gaps, and next recall prompts.** Rationale: reflection plus retrieval transforms a chat into reusable study material. Source: https://www.learningscientists.org/blog/2016/6/23-1

### DON'T
- **Do not answer immediately when the user is trying to learn.** Rationale: answer-first chat encourages passive copying and weakens productive struggle. Source: https://arxiv.org/html/2409.05511v1
- **Do not confuse fluency with mastery.** Rationale: rereading or seeing a polished LLM answer creates an illusion of competence unless the learner retrieves unaided. Source: https://www.retrievalpractice.org/why-it-works
- **Do not use Socratic questioning as a rigid rule for all learners.** Rationale: low-prior-knowledge learners may need explicit worked examples before open-ended questioning. Source: https://research.mental-momentum.ai/r/socratic-ai-tutoring-conceptual-mdgdl9
- **Do not let the model complete the user’s assignment without learner effort.** Rationale: over-reliance on generative AI can reduce critical thinking and skill acquisition. Source: https://www.sciencedirect.com/science/article/pii/S2666920X24000086
- **Do not make flashcards or recall prompts for material the user has not understood.** Rationale: SuperMemo’s first rule is to understand before memorizing. Source: https://www.supermemo.com/en/blog/twenty-rules-of-formulating-knowledge
- **Do not overload the learner with long multi-topic lectures.** Rationale: high extraneous cognitive load reduces learning efficiency. Source: https://www.structural-learning.com/post/cognitive-load-theory-a-teachers-guide
- **Do not reward only correct final answers.** Rationale: tutoring should inspect reasoning steps because LLM-assisted learners can land on correct answers by copying. Source: https://arxiv.org/html/2409.05511v1
- **Do not make the AI the only evaluator.** Rationale: LLM tutors need human/course alignment and evaluation against learning objectives. Source: https://www.psy.uq.edu.au/~uqjtange/academic_ai/t_socratic_tutoring.html

## 2. GPU INFRA / OPS MODE

### DO
- **Generate a runbook before touching production.** Rationale: runbooks externalize operational knowledge into step-by-step procedures usable under stress. Source: https://sre.google/sre-book/postmortem-culture
- **Start every risky command with a dry-run or read-only inspection when the tool supports it.** Rationale: dry runs and prechecks catch scope mistakes before irreversible infra changes. Source: https://oneuptime.com/blog/post/2026-01-30-sre-runbook-automation/view
- **Log commands, timestamps, targets, outputs, and decisions automatically.** Rationale: audit trails and incident timelines reduce archaeology during reviews and compliance work. Source: https://incident.io/blog/runbook-automation-tools-2026-the-complete-guide
- **Attach success criteria and rollback steps to each action.** Rationale: good runbooks define how to know a step worked and how to recover if it did not. Source: https://www.solarwinds.com/sre-best-practices/runbook-automation
- **Use checklists for high-pressure GPU operations.** Rationale: explicit checklists reduce omission errors during paging, deploys, failovers, quota changes, and node drains. Source: https://drdroid.io/guides/runbooks-guide-for-sre-on-call-teams
- **Prefer one complete action per step.** Rationale: runbooks are more reliable when instructions are concise, sequential, and not compound. Source: https://www.harness.io/blog/how-to-build-runbooks-that-work----and-automate-them-with-harness-ai-sre
- **Keep humans in the approval loop for destructive or expensive actions.** Rationale: approval gates limit blast radius for actions that delete data, restart fleets, or spend money. Source: https://oneuptime.com/blog/post/2026-01-30-sre-runbook-automation/view
- **Turn every incident into post-incident notes and action items.** Rationale: SRE postmortems document impact, mitigation, root causes, and prevention work. Source: https://sre.google/sre-book/postmortem-culture
- **Update runbooks immediately after incidents or surprises.** Rationale: postmortem learning only compounds if new facts change operational docs and action items. Source: https://sre.google/workbook/postmortem-culture
- **Version operational docs with code or config.** Rationale: versioned runbooks let teams review, diff, and roll back operational knowledge. Source: https://www.solarwinds.com/sre-best-practices/runbook-automation
- **Capture environment assumptions for GPU work.** Rationale: GPU ops often fail on hidden constraints such as driver/CUDA versions, quota, topology, node health, and scheduler state. Source: https://docs.nvidia.com/datacenter/dcgm/latest/user-guide/feature-overview.html
- **Use observability links inside runbooks.** Rationale: responders need dashboards, logs, traces, and SLO context at the exact step where they decide. Source: https://fatihkoc.net/posts/sre-observability-slo-runbooks

### DON'T
- **Do not run opaque shell snippets without explaining scope and blast radius.** Rationale: responders need to know what resources, regions, clusters, and accounts a command can affect. Source: https://drdroid.io/guides/runbooks-guide-for-sre-on-call-teams
- **Do not automate a procedure before it has been validated manually.** Rationale: SRE guidance favors starting with a manual runbook, then automating stable repeatable steps. Source: https://www.solarwinds.com/sre-best-practices/runbook-automation
- **Do not hide failed attempts or discarded hypotheses.** Rationale: complete timelines help postmortems reconstruct what happened and why. Source: https://incident.io/blog/sre-incident-postmortem-best-practices
- **Do not write blame-oriented incident notes.** Rationale: blameless postmortems improve reliability by focusing on systemic causes and fixes. Source: https://sre.google/sre-book/postmortem-culture
- **Do not let runbooks become stale wiki pages.** Rationale: outdated runbooks create false confidence and increase on-call risk. Source: https://www.harness.io/blog/how-to-build-runbooks-that-work----and-automate-them-with-harness-ai-sre
- **Do not skip rollback tests.** Rationale: an untested rollback is not a reliable recovery path during production pressure. Source: https://oneuptime.com/blog/post/2026-01-30-sre-runbook-automation/view
- **Do not treat observability as a substitute for decisions.** Rationale: metrics and logs need SLOs, severity rules, and runbooks to become reliable action. Source: https://fatihkoc.net/posts/sre-observability-slo-runbooks
- **Do not execute destructive actions without explicit confirmation.** Rationale: human approval gates are a core safeguard for production and cost-impacting operations. Source: https://oneuptime.com/blog/post/2026-01-30-sre-runbook-automation/view

## 3. RETENTION STORE

### DO
- **Extract atomic knowledge units from chat logs.** Rationale: SuperMemo’s minimum-information principle says each review item should test one idea. Source: https://www.supermemo.com/en/blog/twenty-rules-of-formulating-knowledge
- **Generate flashcards only after summarizing the underlying concept.** Rationale: understanding before memorization prevents brittle cards that reinforce meaningless text. Source: https://supermemopedia.com/wiki/20_rules
- **Use cloze deletion for definitions, commands, and small conceptual gaps.** Rationale: cloze cards are fast to create and effective when the omitted fact is precise. Source: https://supermemopedia.com/wiki/20_rules
- **Include source links back to the original chat/session/file.** Rationale: cards and TILs need context for correction, trust, and later elaboration. Source: https://www.supermemo.com/en/blog/twenty-rules-of-formulating-knowledge
- **Store TIL notes as short Markdown entries.** Rationale: developer TIL repositories make small lessons searchable, skimmable, and versionable. Source: https://www.develves.net/blogs/asd/2016-11-27-today-i-learned/
- **Maintain a weekly review queue.** Rationale: periodic review lets the system promote durable lessons, retire noise, and schedule spaced recall. Source: https://fortelabs.com/blog/weekly-review/
- **Separate evergreen concepts from volatile operational facts.** Rationale: stable principles suit flashcards, while changing commands/configs belong in versioned notes or runbooks. Source: https://www.supermemo.com/en/blog/twenty-rules-of-formulating-knowledge
- **Let AI draft cards but require user or rule-based quality checks before publishing.** Rationale: LLM extraction can save time, but card quality depends on atomicity, accuracy, and relevance. Source: https://www.supermemo.com/en/blog/twenty-rules-of-formulating-knowledge
- **Support Markdown/Obsidian/Anki export paths.** Rationale: plain Markdown and Anki-style spaced repetition are proven user-owned review workflows. Source: https://github.com/Pseudonium/Obsidian_to_Anki
- **Prioritize cards by future utility.** Rationale: spaced systems become unmanageable unless low-value material is filtered aggressively. Source: https://www.supermemo.com/en/blog/twenty-rules-of-formulating-knowledge
- **Track review outcomes and edit bad cards.** Rationale: failed or annoying cards usually signal ambiguity, excessive scope, or missing prerequisite knowledge. Source: https://www.supermemo.com/en/blog/twenty-rules-of-formulating-knowledge

### DON'T
- **Do not dump whole chat summaries into flashcards.** Rationale: long, multi-part cards violate minimum information and are hard to recall reliably. Source: https://www.supermemo.com/en/blog/twenty-rules-of-formulating-knowledge
- **Do not retain everything.** Rationale: indiscriminate capture creates review debt and makes high-value knowledge harder to find. Source: https://fortelabs.com/blog/weekly-review/
- **Do not make cards for unresolved or untrusted claims.** Rationale: review systems amplify errors when unverified AI outputs become repeated knowledge. Source: https://www.supermemo.com/en/blog/twenty-rules-of-formulating-knowledge
- **Do not mix secrets or sensitive operational details into the retention store.** Rationale: chat-derived notes need privacy filtering before export or sync. Source: https://owasp.org/www-project-top-10-for-large-language-model-applications/
- **Do not use spaced repetition for frequently changing commands without timestamps.** Rationale: volatile facts should be dated or stored as runbooks to prevent stale recall. Source: https://www.solarwinds.com/sre-best-practices/runbook-automation
- **Do not let AI-generated notes bypass taxonomy.** Rationale: TIL and Obsidian-style systems rely on folders, tags, links, or indexes for retrieval. Source: https://www.develves.net/blogs/asd/2016-11-27-today-i-learned/
- **Do not optimize for card count.** Rationale: a smaller set of understood, useful, atomic cards beats a large pile of low-signal prompts. Source: https://supermemopedia.com/wiki/20_rules
- **Do not detach cards from examples.** Rationale: examples reduce ambiguity and connect abstract knowledge to practical use. Source: https://www.supermemo.com/en/blog/twenty-rules-of-formulating-knowledge

## Design implications for jcode modes

- **Learning Mode should be a stateful tutor, not an answer wrapper:** default to questions, hints, learner generation, short explanations, and explicit understanding checks.
- **Learning Mode needs an escape hatch:** when the user is blocked or under time pressure, switch from Socratic mode to worked examples or direct answers, while labeling the tradeoff.
- **Learning Mode should emit retention artifacts:** end sessions with concepts learned, gaps, recall prompts, and suggested spaced-review dates.
- **GPU Infra/Ops Mode should be safety-first:** plan, inspect, dry-run, confirm, execute, log, verify, and update docs.
- **GPU Infra/Ops Mode should maintain an operation transcript:** commands, outputs, decisions, observability links, rollback plans, and post-incident notes should be captured automatically.
- **Retention Store should classify chat-derived knowledge:** flashcards for stable atomic concepts, TILs for useful snippets, runbooks for operational procedures, and follow-up tasks for unresolved items.
- **Retention Store should include quality gates:** no secrets, no unverified claims, no multi-fact cards, no stale operational commands without date/context.
- **jcode should make modes composable:** a GPU debugging session can produce a runbook plus a few learning cards, while a learning session can produce TILs and review prompts.

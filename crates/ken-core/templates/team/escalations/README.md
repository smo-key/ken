# escalations/

One file per escalation, written from `ESCALATION.md`. An escalation raises a
decision to the person who owns it. Anyone can raise one: an agent that cannot
decide, two agents that cannot settle a disagreement, or a person whose
decision belongs to someone else. A request asks for work and becomes a ticket
once it is accepted; an escalation asks for a decision.

The person it goes to answers in the thread at the bottom of the file. Each
reply is added under the last one, and the thread keeps going until the
question is settled. The person who raised it sees every reply. An agent that
raised it waits, and resumes on the answer.

The answer then goes where it belongs: into the ticket, into the context pack
for the topic, or, if it is a ruling, into the decisions log the same day. The
Answer drift check flags an answered escalation that reached neither the pack
nor the decisions log.

| status | means |
|---|---|
| `open` | waiting for an answer |
| `answered` | the question is settled · `answer` says where it was recorded |
| `withdrawn` | the raiser no longer needs it · the thread says why |

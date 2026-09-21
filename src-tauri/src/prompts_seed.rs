//! The built-in practice corpus.
//!
//! Compiled into the binary rather than downloaded: the whole point of the
//! app is that it works with no network, and a first run with an empty
//! prompt list is indistinguishable from a broken install.
//!
//! Two kinds of prompt:
//!
//! - **Read-aloud** — `target_text` is set, so the recording can be compared
//!   word by word against a known reference and scored.
//! - **Open-ended** — `target_text` is `None`. There is no reference, so
//!   pronunciation is never scored; the feedback is grammar and fluency
//!   only. Inventing a pronunciation number for free speech would be a
//!   fabricated measurement, and the UI says so instead.
//!
//! Ids are deterministic (`builtin-{category}-{n}`) and seeding is
//! `INSERT OR IGNORE`, so adding prompts in a later release tops up an
//! existing database without touching anything a user has practised.

/// One compiled-in prompt.
pub struct BuiltinPrompt {
    pub id: &'static str,
    pub category: &'static str,
    pub topic: &'static str,
    /// What the screen shows: either framing for a read-aloud, or the
    /// question itself for an open-ended prompt.
    pub prompt_text: &'static str,
    /// The exact words to say, when there are any.
    pub target_text: Option<&'static str>,
    /// 1 beginner, 2 intermediate, 3 advanced.
    pub level: i64,
}

const fn read(
    id: &'static str,
    category: &'static str,
    topic: &'static str,
    prompt_text: &'static str,
    target_text: &'static str,
    level: i64,
) -> BuiltinPrompt {
    BuiltinPrompt {
        id,
        category,
        topic,
        prompt_text,
        target_text: Some(target_text),
        level,
    }
}

const fn open(
    id: &'static str,
    category: &'static str,
    topic: &'static str,
    prompt_text: &'static str,
    level: i64,
) -> BuiltinPrompt {
    BuiltinPrompt {
        id,
        category,
        topic,
        prompt_text,
        target_text: None,
        level,
    }
}

pub const CATEGORY_CONVERSATION: &str = "conversation";
pub const CATEGORY_INTERVIEW: &str = "interview";

pub const BUILTIN_PROMPTS: &[BuiltinPrompt] = &[
    // ─── Conversation: greetings and small talk ─────────────────────────
    read(
        "builtin-conversation-1",
        CATEGORY_CONVERSATION,
        "small talk",
        "Someone you have met once before says hello. Reply:",
        "Hi, good to see you again. How have you been?",
        1,
    ),
    read(
        "builtin-conversation-2",
        CATEGORY_CONVERSATION,
        "small talk",
        "A neighbour asks how your weekend was. Say:",
        "It was quiet, thanks. I finally caught up on sleep.",
        1,
    ),
    read(
        "builtin-conversation-3",
        CATEGORY_CONVERSATION,
        "small talk",
        "Make small talk about the weather:",
        "It has been raining all week, but I hear it clears up tomorrow.",
        1,
    ),
    read(
        "builtin-conversation-4",
        CATEGORY_CONVERSATION,
        "small talk",
        "You are leaving a conversation politely. Say:",
        "It was really nice talking to you. I should get going.",
        1,
    ),
    read(
        "builtin-conversation-5",
        CATEGORY_CONVERSATION,
        "small talk",
        "Introduce yourself to someone at a meetup:",
        "Hi, I do not think we have met. I am here with a friend.",
        1,
    ),
    open(
        "builtin-conversation-6",
        CATEGORY_CONVERSATION,
        "small talk",
        "Describe your typical morning, from waking up to leaving the house.",
        1,
    ),
    open(
        "builtin-conversation-7",
        CATEGORY_CONVERSATION,
        "small talk",
        "What did you do last weekend? Talk for about thirty seconds.",
        1,
    ),
    open(
        "builtin-conversation-8",
        CATEGORY_CONVERSATION,
        "small talk",
        "Tell me about the area you live in. What is good about it?",
        2,
    ),
    // ─── Conversation: ordering and eating out ──────────────────────────
    read(
        "builtin-conversation-9",
        CATEGORY_CONVERSATION,
        "ordering",
        "At a cafe, order a drink:",
        "Could I get a large coffee with milk, please?",
        1,
    ),
    read(
        "builtin-conversation-10",
        CATEGORY_CONVERSATION,
        "ordering",
        "Ask about something on the menu:",
        "Excuse me, does the soup have any dairy in it?",
        2,
    ),
    read(
        "builtin-conversation-11",
        CATEGORY_CONVERSATION,
        "ordering",
        "Order for two people:",
        "We will have the chicken and the pasta, and a jug of water.",
        2,
    ),
    read(
        "builtin-conversation-12",
        CATEGORY_CONVERSATION,
        "ordering",
        "Ask for the bill:",
        "Could we get the bill when you have a moment, please?",
        1,
    ),
    read(
        "builtin-conversation-13",
        CATEGORY_CONVERSATION,
        "ordering",
        "Send something back politely:",
        "Sorry to bother you, but I think this is the wrong order.",
        2,
    ),
    open(
        "builtin-conversation-14",
        CATEGORY_CONVERSATION,
        "ordering",
        "Describe your favourite meal and how it is made.",
        2,
    ),
    // ─── Conversation: shopping ─────────────────────────────────────────
    read(
        "builtin-conversation-15",
        CATEGORY_CONVERSATION,
        "shopping",
        "Ask a shop assistant for help:",
        "Excuse me, do you have this in a smaller size?",
        1,
    ),
    read(
        "builtin-conversation-16",
        CATEGORY_CONVERSATION,
        "shopping",
        "Return something:",
        "I would like to return this, please. I still have the receipt.",
        2,
    ),
    read(
        "builtin-conversation-17",
        CATEGORY_CONVERSATION,
        "shopping",
        "Ask about a price:",
        "Is this the sale price, or does the discount come off at the till?",
        2,
    ),
    read(
        "builtin-conversation-18",
        CATEGORY_CONVERSATION,
        "shopping",
        "Decline help politely:",
        "Thanks, I am just looking for now.",
        1,
    ),
    open(
        "builtin-conversation-19",
        CATEGORY_CONVERSATION,
        "shopping",
        "Describe something you bought recently and whether it was worth it.",
        2,
    ),
    // ─── Conversation: directions and travel ────────────────────────────
    read(
        "builtin-conversation-20",
        CATEGORY_CONVERSATION,
        "travel",
        "Ask for directions:",
        "Sorry, could you tell me how to get to the train station?",
        1,
    ),
    read(
        "builtin-conversation-21",
        CATEGORY_CONVERSATION,
        "travel",
        "Give directions:",
        "Go straight for two blocks, then turn left at the traffic lights.",
        1,
    ),
    read(
        "builtin-conversation-22",
        CATEGORY_CONVERSATION,
        "travel",
        "Check in at a hotel:",
        "Hello, I have a reservation under my last name for two nights.",
        2,
    ),
    read(
        "builtin-conversation-23",
        CATEGORY_CONVERSATION,
        "travel",
        "Ask about a delayed flight:",
        "Excuse me, has there been any update on the delay?",
        2,
    ),
    read(
        "builtin-conversation-24",
        CATEGORY_CONVERSATION,
        "travel",
        "Buy a ticket:",
        "One return ticket to the city centre, please. When is the next one?",
        2,
    ),
    open(
        "builtin-conversation-25",
        CATEGORY_CONVERSATION,
        "travel",
        "Describe a trip you have taken. Where did you go and what happened?",
        2,
    ),
    open(
        "builtin-conversation-26",
        CATEGORY_CONVERSATION,
        "travel",
        "You have missed your connection. Explain the situation to a staff member.",
        3,
    ),
    // ─── Conversation: phone calls and appointments ─────────────────────
    read(
        "builtin-conversation-27",
        CATEGORY_CONVERSATION,
        "phone calls",
        "Answer a call from an unknown number:",
        "Hello, this is speaking. How can I help you?",
        1,
    ),
    read(
        "builtin-conversation-28",
        CATEGORY_CONVERSATION,
        "phone calls",
        "Book an appointment by phone:",
        "Hi, I would like to book an appointment for sometime next week.",
        2,
    ),
    read(
        "builtin-conversation-29",
        CATEGORY_CONVERSATION,
        "phone calls",
        "Ask someone to repeat themselves:",
        "Sorry, the line is not great. Could you say that again?",
        1,
    ),
    read(
        "builtin-conversation-30",
        CATEGORY_CONVERSATION,
        "phone calls",
        "Leave a voicemail:",
        "Hi, it is me calling about the delivery. Please give me a ring back.",
        2,
    ),
    read(
        "builtin-conversation-31",
        CATEGORY_CONVERSATION,
        "phone calls",
        "Reschedule:",
        "Something has come up, so I need to move our appointment if possible.",
        2,
    ),
    open(
        "builtin-conversation-32",
        CATEGORY_CONVERSATION,
        "phone calls",
        "Call a company to ask why your order has not arrived. Explain the problem.",
        3,
    ),
    // ─── Conversation: health ───────────────────────────────────────────
    read(
        "builtin-conversation-33",
        CATEGORY_CONVERSATION,
        "health",
        "Describe a symptom to a doctor:",
        "I have had a sore throat and a headache for about three days.",
        2,
    ),
    read(
        "builtin-conversation-34",
        CATEGORY_CONVERSATION,
        "health",
        "Ask about medication:",
        "How often should I take this, and should I take it with food?",
        2,
    ),
    read(
        "builtin-conversation-35",
        CATEGORY_CONVERSATION,
        "health",
        "Ask for a pharmacy recommendation:",
        "Is there anything you would recommend for a blocked nose?",
        2,
    ),
    open(
        "builtin-conversation-36",
        CATEGORY_CONVERSATION,
        "health",
        "Explain to a doctor how you injured yourself and what hurts now.",
        3,
    ),
    // ─── Conversation: at work ──────────────────────────────────────────
    read(
        "builtin-conversation-37",
        CATEGORY_CONVERSATION,
        "workplace",
        "Greet a colleague on Monday:",
        "Morning. Did you get a chance to look at that email I sent?",
        1,
    ),
    read(
        "builtin-conversation-38",
        CATEGORY_CONVERSATION,
        "workplace",
        "Ask for help:",
        "Do you have ten minutes at some point today? I am stuck on something.",
        2,
    ),
    read(
        "builtin-conversation-39",
        CATEGORY_CONVERSATION,
        "workplace",
        "Give a short status update:",
        "The first part is finished. The rest should be ready by Thursday.",
        2,
    ),
    read(
        "builtin-conversation-40",
        CATEGORY_CONVERSATION,
        "workplace",
        "Ask for time off:",
        "I wanted to check whether taking Friday off would cause any problems.",
        2,
    ),
    read(
        "builtin-conversation-41",
        CATEGORY_CONVERSATION,
        "workplace",
        "Chase something without sounding annoyed:",
        "Just circling back on this one. Is there anything you need from me?",
        3,
    ),
    open(
        "builtin-conversation-42",
        CATEGORY_CONVERSATION,
        "workplace",
        "Explain what you do at work to someone outside your field.",
        2,
    ),
    open(
        "builtin-conversation-43",
        CATEGORY_CONVERSATION,
        "workplace",
        "A deadline is going to slip. Tell your manager, and say what you propose.",
        3,
    ),
    // ─── Conversation: disagreeing and complaining ──────────────────────
    read(
        "builtin-conversation-44",
        CATEGORY_CONVERSATION,
        "disagreeing",
        "Disagree politely:",
        "I see what you mean, but I am not sure that would work for us.",
        2,
    ),
    read(
        "builtin-conversation-45",
        CATEGORY_CONVERSATION,
        "disagreeing",
        "Push back on a decision:",
        "Can I offer a different angle on that before we decide?",
        3,
    ),
    read(
        "builtin-conversation-46",
        CATEGORY_CONVERSATION,
        "disagreeing",
        "Complain about a service:",
        "I have been waiting over an hour, and nobody has been able to help.",
        2,
    ),
    read(
        "builtin-conversation-47",
        CATEGORY_CONVERSATION,
        "disagreeing",
        "Ask for something to be fixed:",
        "I would really appreciate it if this could be sorted out today.",
        2,
    ),
    read(
        "builtin-conversation-48",
        CATEGORY_CONVERSATION,
        "disagreeing",
        "Hold your position without escalating:",
        "I understand that is the policy, but it does not solve my problem.",
        3,
    ),
    open(
        "builtin-conversation-49",
        CATEGORY_CONVERSATION,
        "disagreeing",
        "A friend suggests a plan you do not want to join. Turn it down kindly.",
        2,
    ),
    open(
        "builtin-conversation-50",
        CATEGORY_CONVERSATION,
        "disagreeing",
        "Describe a time you disagreed with someone and how it was resolved.",
        3,
    ),
    // ─── Conversation: apologies and plans ──────────────────────────────
    read(
        "builtin-conversation-51",
        CATEGORY_CONVERSATION,
        "apologising",
        "Apologise for being late:",
        "I am so sorry I am late. The traffic was much worse than I expected.",
        1,
    ),
    read(
        "builtin-conversation-52",
        CATEGORY_CONVERSATION,
        "apologising",
        "Apologise for a mistake at work:",
        "That was my mistake, and I have already started fixing it.",
        2,
    ),
    read(
        "builtin-conversation-53",
        CATEGORY_CONVERSATION,
        "making plans",
        "Invite someone out:",
        "Are you free on Saturday? A few of us are going for dinner.",
        1,
    ),
    read(
        "builtin-conversation-54",
        CATEGORY_CONVERSATION,
        "making plans",
        "Suggest a different time:",
        "Saturday is tricky for me. Would Sunday afternoon work instead?",
        2,
    ),
    read(
        "builtin-conversation-55",
        CATEGORY_CONVERSATION,
        "making plans",
        "Confirm arrangements:",
        "So that is seven o clock at the place near the station. See you there.",
        2,
    ),
    read(
        "builtin-conversation-56",
        CATEGORY_CONVERSATION,
        "making plans",
        "Cancel at short notice:",
        "I am really sorry, but I am not going to make it tonight.",
        2,
    ),
    open(
        "builtin-conversation-57",
        CATEGORY_CONVERSATION,
        "making plans",
        "Plan a day out for a visiting friend. Say where you would take them and why.",
        2,
    ),
    open(
        "builtin-conversation-58",
        CATEGORY_CONVERSATION,
        "small talk",
        "Tell a short story about something that happened to you recently.",
        3,
    ),
    open(
        "builtin-conversation-59",
        CATEGORY_CONVERSATION,
        "small talk",
        "Explain a hobby of yours to someone who has never tried it.",
        2,
    ),
    open(
        "builtin-conversation-60",
        CATEGORY_CONVERSATION,
        "small talk",
        "Describe someone you admire and what you have learned from them.",
        3,
    ),
    // ─── Interview: opening and self-introduction ───────────────────────
    read(
        "builtin-interview-1",
        CATEGORY_INTERVIEW,
        "opening",
        "Greet the interviewer:",
        "Thanks for making the time today. It is good to meet you.",
        1,
    ),
    read(
        "builtin-interview-2",
        CATEGORY_INTERVIEW,
        "opening",
        "Open a one-line summary of yourself:",
        "I have spent the last four years working on customer facing products.",
        2,
    ),
    read(
        "builtin-interview-3",
        CATEGORY_INTERVIEW,
        "opening",
        "Bridge from your background to the role:",
        "That experience lines up closely with what this role seems to need.",
        2,
    ),
    open(
        "builtin-interview-4",
        CATEGORY_INTERVIEW,
        "opening",
        "Tell me about yourself. Keep it under ninety seconds.",
        1,
    ),
    open(
        "builtin-interview-5",
        CATEGORY_INTERVIEW,
        "opening",
        "Walk me through your resume, most recent role first.",
        2,
    ),
    open(
        "builtin-interview-6",
        CATEGORY_INTERVIEW,
        "opening",
        "Why are you looking for a new role right now?",
        2,
    ),
    open(
        "builtin-interview-7",
        CATEGORY_INTERVIEW,
        "opening",
        "What do you know about what we do here?",
        2,
    ),
    // ─── Interview: experience and behavioural ──────────────────────────
    read(
        "builtin-interview-8",
        CATEGORY_INTERVIEW,
        "behavioural",
        "Set up the situation in a STAR answer:",
        "Last year our team took over a project that was already behind schedule.",
        2,
    ),
    read(
        "builtin-interview-9",
        CATEGORY_INTERVIEW,
        "behavioural",
        "State your specific task:",
        "My job was to work out what was blocking us and get us back on track.",
        2,
    ),
    read(
        "builtin-interview-10",
        CATEGORY_INTERVIEW,
        "behavioural",
        "Describe the action you took:",
        "I mapped out every dependency and cut the two that were not essential.",
        3,
    ),
    read(
        "builtin-interview-11",
        CATEGORY_INTERVIEW,
        "behavioural",
        "Give the result with a number in it:",
        "We shipped three weeks later, and support tickets dropped by about half.",
        3,
    ),
    open(
        "builtin-interview-12",
        CATEGORY_INTERVIEW,
        "behavioural",
        "Tell me about a time you had to deliver under a tight deadline.",
        2,
    ),
    open(
        "builtin-interview-13",
        CATEGORY_INTERVIEW,
        "behavioural",
        "Describe a project you are proud of. What was your specific contribution?",
        2,
    ),
    open(
        "builtin-interview-14",
        CATEGORY_INTERVIEW,
        "behavioural",
        "Tell me about a time you had to learn something quickly.",
        2,
    ),
    open(
        "builtin-interview-15",
        CATEGORY_INTERVIEW,
        "behavioural",
        "Give an example of a decision you made with incomplete information.",
        3,
    ),
    open(
        "builtin-interview-16",
        CATEGORY_INTERVIEW,
        "behavioural",
        "Tell me about a time you changed your mind after hearing an argument.",
        3,
    ),
    // ─── Interview: teamwork and conflict ───────────────────────────────
    read(
        "builtin-interview-17",
        CATEGORY_INTERVIEW,
        "teamwork",
        "Describe how you work with others:",
        "I try to ask questions early rather than guess and rework things later.",
        2,
    ),
    read(
        "builtin-interview-18",
        CATEGORY_INTERVIEW,
        "teamwork",
        "Frame a disagreement without blaming anyone:",
        "We wanted the same outcome, but we disagreed about the order of the work.",
        3,
    ),
    open(
        "builtin-interview-19",
        CATEGORY_INTERVIEW,
        "teamwork",
        "Tell me about a conflict with a colleague and how you handled it.",
        2,
    ),
    open(
        "builtin-interview-20",
        CATEGORY_INTERVIEW,
        "teamwork",
        "Describe a time you had to give someone difficult feedback.",
        3,
    ),
    open(
        "builtin-interview-21",
        CATEGORY_INTERVIEW,
        "teamwork",
        "How do you work with people whose style is very different from yours?",
        2,
    ),
    open(
        "builtin-interview-22",
        CATEGORY_INTERVIEW,
        "teamwork",
        "Tell me about a time you helped someone else succeed.",
        2,
    ),
    // ─── Interview: failure and growth ──────────────────────────────────
    read(
        "builtin-interview-23",
        CATEGORY_INTERVIEW,
        "failure",
        "Own a mistake without over-apologising:",
        "I misjudged how long the migration would take, and I said so early.",
        3,
    ),
    read(
        "builtin-interview-24",
        CATEGORY_INTERVIEW,
        "failure",
        "Say what changed afterwards:",
        "Since then I break estimates down before committing to a date.",
        2,
    ),
    open(
        "builtin-interview-25",
        CATEGORY_INTERVIEW,
        "failure",
        "Tell me about a time you failed. What did you take from it?",
        2,
    ),
    open(
        "builtin-interview-26",
        CATEGORY_INTERVIEW,
        "failure",
        "What is the hardest feedback you have received, and what did you do?",
        3,
    ),
    open(
        "builtin-interview-27",
        CATEGORY_INTERVIEW,
        "failure",
        "Describe something you are actively trying to get better at.",
        2,
    ),
    // ─── Interview: strengths, weaknesses, motivation ───────────────────
    read(
        "builtin-interview-28",
        CATEGORY_INTERVIEW,
        "strengths",
        "Claim a strength with evidence attached:",
        "I am good at untangling problems that nobody has written down yet.",
        3,
    ),
    read(
        "builtin-interview-29",
        CATEGORY_INTERVIEW,
        "strengths",
        "Give a real weakness and the guard rail you use:",
        "I go too deep on detail, so I set a time limit before I start.",
        3,
    ),
    open(
        "builtin-interview-30",
        CATEGORY_INTERVIEW,
        "strengths",
        "What would your last manager say you are best at?",
        2,
    ),
    open(
        "builtin-interview-31",
        CATEGORY_INTERVIEW,
        "strengths",
        "What is your biggest weakness? Be specific and honest.",
        2,
    ),
    open(
        "builtin-interview-32",
        CATEGORY_INTERVIEW,
        "motivation",
        "Why do you want this job in particular?",
        1,
    ),
    open(
        "builtin-interview-33",
        CATEGORY_INTERVIEW,
        "motivation",
        "What kind of work makes you lose track of time?",
        2,
    ),
    open(
        "builtin-interview-34",
        CATEGORY_INTERVIEW,
        "motivation",
        "Where would you like to be in three years?",
        2,
    ),
    // ─── Interview: explaining your work ────────────────────────────────
    read(
        "builtin-interview-35",
        CATEGORY_INTERVIEW,
        "explaining",
        "Start a technical explanation for a non-expert:",
        "The simplest way to think about it is a queue with one door.",
        3,
    ),
    read(
        "builtin-interview-36",
        CATEGORY_INTERVIEW,
        "explaining",
        "Check that you are being understood:",
        "Does that make sense so far, or should I go over that part again?",
        2,
    ),
    open(
        "builtin-interview-37",
        CATEGORY_INTERVIEW,
        "explaining",
        "Explain the most complicated thing you have built, to someone non-technical.",
        3,
    ),
    open(
        "builtin-interview-38",
        CATEGORY_INTERVIEW,
        "explaining",
        "Walk me through how you would approach a problem you have never seen before.",
        3,
    ),
    open(
        "builtin-interview-39",
        CATEGORY_INTERVIEW,
        "explaining",
        "Teach me something you know well, in one minute.",
        2,
    ),
    // ─── Interview: pressure and edge cases ─────────────────────────────
    read(
        "builtin-interview-40",
        CATEGORY_INTERVIEW,
        "pressure",
        "Buy yourself a moment without filler words:",
        "That is a good question. Let me think about it for a second.",
        1,
    ),
    read(
        "builtin-interview-41",
        CATEGORY_INTERVIEW,
        "pressure",
        "Admit you do not know something:",
        "I have not worked with that directly, but here is how I would start.",
        2,
    ),
    read(
        "builtin-interview-42",
        CATEGORY_INTERVIEW,
        "pressure",
        "Ask for clarification:",
        "Just so I answer the right question, do you mean the technical side?",
        2,
    ),
    open(
        "builtin-interview-43",
        CATEGORY_INTERVIEW,
        "pressure",
        "You are asked about a gap in your work history. Explain it.",
        3,
    ),
    open(
        "builtin-interview-44",
        CATEGORY_INTERVIEW,
        "pressure",
        "You are asked about a skill on the job description you do not have. Answer.",
        3,
    ),
    open(
        "builtin-interview-45",
        CATEGORY_INTERVIEW,
        "pressure",
        "Why did you leave your last role?",
        2,
    ),
    // ─── Interview: money ───────────────────────────────────────────────
    read(
        "builtin-interview-46",
        CATEGORY_INTERVIEW,
        "salary",
        "Defer the salary question early on:",
        "I would rather understand the role first, if that is alright.",
        2,
    ),
    read(
        "builtin-interview-47",
        CATEGORY_INTERVIEW,
        "salary",
        "State a range with a reason:",
        "Based on the market and my experience, I am looking in that range.",
        3,
    ),
    read(
        "builtin-interview-48",
        CATEGORY_INTERVIEW,
        "salary",
        "Negotiate without closing the door:",
        "I am excited about this. Is there any flexibility on the base?",
        3,
    ),
    read(
        "builtin-interview-49",
        CATEGORY_INTERVIEW,
        "salary",
        "Ask for time to consider an offer:",
        "Thank you. Could I have a couple of days to think it over?",
        2,
    ),
    open(
        "builtin-interview-50",
        CATEGORY_INTERVIEW,
        "salary",
        "You are asked your current salary. Answer without giving a number.",
        3,
    ),
    open(
        "builtin-interview-51",
        CATEGORY_INTERVIEW,
        "salary",
        "An offer comes in below your range. Respond out loud.",
        3,
    ),
    // ─── Interview: closing ─────────────────────────────────────────────
    read(
        "builtin-interview-52",
        CATEGORY_INTERVIEW,
        "closing",
        "Ask about the team:",
        "What does a typical week look like for someone in this role?",
        1,
    ),
    read(
        "builtin-interview-53",
        CATEGORY_INTERVIEW,
        "closing",
        "Ask about success:",
        "How would you know, six months in, that this hire went well?",
        2,
    ),
    read(
        "builtin-interview-54",
        CATEGORY_INTERVIEW,
        "closing",
        "Ask about the hard parts:",
        "What is the part of this job that people find most difficult?",
        2,
    ),
    read(
        "builtin-interview-55",
        CATEGORY_INTERVIEW,
        "closing",
        "Ask about next steps:",
        "What are the next steps, and when should I expect to hear back?",
        1,
    ),
    read(
        "builtin-interview-56",
        CATEGORY_INTERVIEW,
        "closing",
        "Close with interest:",
        "This sounds like the kind of work I want to be doing. Thank you.",
        2,
    ),
    open(
        "builtin-interview-57",
        CATEGORY_INTERVIEW,
        "closing",
        "What questions do you have for us?",
        1,
    ),
    open(
        "builtin-interview-58",
        CATEGORY_INTERVIEW,
        "closing",
        "Summarise, in thirty seconds, why you are a good fit for this role.",
        3,
    ),
    open(
        "builtin-interview-59",
        CATEGORY_INTERVIEW,
        "closing",
        "Is there anything we have not asked that you want us to know?",
        2,
    ),
    open(
        "builtin-interview-60",
        CATEGORY_INTERVIEW,
        "closing",
        "Leave a short voicemail following up after an interview.",
        2,
    ),
];

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn corpus_has_both_categories_in_bulk() {
        let conv = BUILTIN_PROMPTS
            .iter()
            .filter(|p| p.category == CATEGORY_CONVERSATION)
            .count();
        let intv = BUILTIN_PROMPTS
            .iter()
            .filter(|p| p.category == CATEGORY_INTERVIEW)
            .count();
        assert_eq!(conv, 60);
        assert_eq!(intv, 60);
        assert_eq!(conv + intv, BUILTIN_PROMPTS.len());
    }

    #[test]
    fn ids_are_unique() {
        // Seeding is INSERT OR IGNORE keyed on id, so a duplicate would
        // silently drop a prompt rather than fail loudly.
        let ids: HashSet<&str> = BUILTIN_PROMPTS.iter().map(|p| p.id).collect();
        assert_eq!(ids.len(), BUILTIN_PROMPTS.len(), "duplicate prompt id");
    }

    #[test]
    fn ids_follow_the_documented_scheme() {
        for p in BUILTIN_PROMPTS {
            assert!(
                p.id.starts_with(&format!("builtin-{}-", p.category)),
                "{} does not match builtin-{{category}}-{{n}}",
                p.id
            );
        }
    }

    #[test]
    fn no_field_is_blank() {
        for p in BUILTIN_PROMPTS {
            assert!(!p.topic.trim().is_empty(), "{} has no topic", p.id);
            assert!(!p.prompt_text.trim().is_empty(), "{} has no text", p.id);
            if let Some(t) = p.target_text {
                assert!(!t.trim().is_empty(), "{} has a blank target", p.id);
            }
        }
    }

    #[test]
    fn levels_are_in_range() {
        for p in BUILTIN_PROMPTS {
            assert!((1..=3).contains(&p.level), "{} has level {}", p.id, p.level);
        }
    }

    #[test]
    fn every_target_survives_tokenization() {
        // A target made only of punctuation would produce an empty reference
        // and an alignment that scores 100% for saying nothing.
        for p in BUILTIN_PROMPTS {
            let Some(target) = p.target_text else {
                continue;
            };
            assert!(
                !crate::pronounce::tokenize(target).is_empty(),
                "{} tokenizes to nothing",
                p.id
            );
        }
    }

    #[test]
    fn both_kinds_of_prompt_exist_in_each_category() {
        for category in [CATEGORY_CONVERSATION, CATEGORY_INTERVIEW] {
            let in_cat: Vec<_> = BUILTIN_PROMPTS
                .iter()
                .filter(|p| p.category == category)
                .collect();
            assert!(
                in_cat.iter().any(|p| p.target_text.is_some()),
                "{category} has no scoreable read-aloud prompts"
            );
            assert!(
                in_cat.iter().any(|p| p.target_text.is_none()),
                "{category} has no open-ended prompts"
            );
        }
    }
}

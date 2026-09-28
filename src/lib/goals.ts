/** The practice goals offered in Onboarding and Settings. One list, so the two cannot drift. */
export const GOALS = [
  {
    id: "everyday",
    label: "Everyday conversation",
    detail: "Small talk, phone calls, ordering, disagreeing politely.",
  },
  {
    id: "interview",
    label: "Job interviews",
    detail: "Tell me about yourself, behavioural answers, salary questions.",
  },
  {
    id: "both",
    label: "Both",
    detail: "Mix prompts from everyday life and interviews.",
  },
] as const;

export type GoalId = (typeof GOALS)[number]["id"];

/** The prompt category Practice opens on. "Both" starts with conversation; the toggle is one tap away. */
export function categoryForGoal(goal: string): "conversation" | "interview" {
  return goal === "interview" ? "interview" : "conversation";
}

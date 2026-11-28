//! Conservative, display-only summaries of goal assessments.

use jcode_base::todo::TodoGoal;

/// Collapse goal assessments into one overall display row.
///
/// Unknown quality or delivery values remain unknown, rather than allowing a
/// completed lane to hide an unassessed or unfinished lane. This synthetic goal
/// must not be persisted or used to decide whether individual lanes may stop.
pub fn aggregate(goals: &[TodoGoal]) -> Option<TodoGoal> {
    let first = goals.first()?;
    if goals.len() == 1 {
        return Some(first.clone());
    }

    // Option's ordering deliberately makes None weaker than every known value.
    let mut summary = TodoGoal {
        closed_feedback_loop: goals
            .iter()
            .map(|goal| goal.closed_feedback_loop)
            .min()
            .flatten(),
        feedback_loop_relevance: goals
            .iter()
            .map(|goal| goal.feedback_loop_relevance)
            .min()
            .flatten(),
        feedback_loop_coverage: goals
            .iter()
            .map(|goal| goal.feedback_loop_coverage)
            .min()
            .flatten(),
        feedback_loop_traceability: goals
            .iter()
            .map(|goal| goal.feedback_loop_traceability)
            .min()
            .flatten(),
        delivery_state: goals.iter().map(|goal| goal.delivery_state).min().flatten(),
        autonomy: goals.iter().map(|goal| goal.autonomy).min().flatten(),
        // Difficulty sets requirements, not progress. Keep the strictest known
        // requirement even when another lane has not assessed its difficulty.
        difficulty: goals.iter().filter_map(|goal| goal.difficulty).max(),
        iteration_maturity: goals
            .iter()
            .map(|goal| goal.iteration_maturity)
            .min()
            .flatten(),
        feedback_loop: combined_text(goals, |goal| goal.feedback_loop.as_deref()),
        stopping_evidence: combined_text(goals, |goal| goal.stopping_evidence.as_deref()),
        // One impossible lane does not make every other lane impossible. Only
        // carry this stop signal when every lane provides a nonempty reason.
        cannot_complete: combined_text(goals, |goal| goal.cannot_complete.as_deref()),
        // Histories from different lanes are not an overall time series.
        ..TodoGoal::default()
    };
    // Terminal maturity variants are different stopping reasons, not ordered
    // degrees of success. Do not invent one shared reason for mixed outcomes.
    if summary
        .iteration_maturity
        .is_some_and(|state| state.permits_completion())
        && goals
            .iter()
            .any(|goal| goal.iteration_maturity != summary.iteration_maturity)
    {
        summary.iteration_maturity = None;
    }
    Some(summary)
}

fn combined_text(goals: &[TodoGoal], field: fn(&TodoGoal) -> Option<&str>) -> Option<String> {
    let entries: Option<Vec<_>> = goals
        .iter()
        .enumerate()
        .map(|(index, goal)| {
            let text = field(goal)?.trim();
            if text.is_empty() {
                return None;
            }
            let label = goal
                .group
                .as_deref()
                .map(str::trim)
                .filter(|label| !label.is_empty())
                .map(str::to_owned)
                .unwrap_or_else(|| format!("Goal {}", index + 1));
            Some(format!("{label}: {text}"))
        })
        .collect();
    entries.map(|entries| entries.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use jcode_base::todo::{
        Autonomy, DeliveryState, Difficulty, FeedbackLoopCoverage, FeedbackLoopRelevance,
        FeedbackLoopState, FeedbackLoopTraceability, IterationMaturity,
    };

    fn completed() -> TodoGoal {
        TodoGoal {
            group: Some("Completed lane".into()),
            closed_feedback_loop: Some(FeedbackLoopState::Closed),
            feedback_loop_relevance: Some(FeedbackLoopRelevance::AcceptanceAligned),
            feedback_loop_coverage: Some(FeedbackLoopCoverage::EdgeAndIntegrationPaths),
            feedback_loop_traceability: Some(FeedbackLoopTraceability::Complete),
            delivery_state: Some(DeliveryState::OutcomeDelivered),
            difficulty: Some(Difficulty::Involved),
            autonomy: Some(Autonomy::Stewardship),
            iteration_maturity: Some(IterationMaturity::OutcomeReached),
            feedback_loop: Some("Integration tests".into()),
            stopping_evidence: Some("All acceptance checks passed".into()),
            ..TodoGoal::default()
        }
    }

    #[test]
    fn zero_goals_have_no_summary() {
        assert_eq!(aggregate(&[]), None);
    }

    #[test]
    fn one_goal_preserves_every_field() {
        let mut goal = completed();
        goal.closed_feedback_loop_history =
            vec![FeedbackLoopState::Weak, FeedbackLoopState::Closed];
        goal.cannot_complete = Some("External constraint".into());
        assert_eq!(aggregate(&[goal.clone()]), Some(goal));
    }

    #[test]
    fn completed_lane_cannot_hide_in_progress_lane() {
        let mut active = completed();
        active.group = Some("Active lane".into());
        active.closed_feedback_loop = Some(FeedbackLoopState::Weak);
        active.feedback_loop_relevance = Some(FeedbackLoopRelevance::Synthetic);
        active.feedback_loop_coverage = Some(FeedbackLoopCoverage::Narrow);
        active.feedback_loop_traceability = Some(FeedbackLoopTraceability::Unmapped);
        active.delivery_state = Some(DeliveryState::ChangeMade);
        active.autonomy = Some(Autonomy::RequestedOnly);
        active.iteration_maturity = Some(IterationMaturity::Improving);
        let summary = aggregate(&[completed(), active.clone()]).unwrap();
        assert_eq!(summary.closed_feedback_loop, active.closed_feedback_loop);
        assert_eq!(
            summary.feedback_loop_relevance,
            active.feedback_loop_relevance
        );
        assert_eq!(
            summary.feedback_loop_coverage,
            active.feedback_loop_coverage
        );
        assert_eq!(
            summary.feedback_loop_traceability,
            active.feedback_loop_traceability
        );
        assert_eq!(summary.delivery_state, active.delivery_state);
        assert_eq!(summary.autonomy, active.autonomy);
        assert_eq!(summary.iteration_maturity, active.iteration_maturity);
        assert!(summary.group.is_none());
        assert!(summary.closed_feedback_loop_history.is_empty());
        assert_eq!(
            aggregate(&[active.clone(), completed()])
                .unwrap()
                .delivery_state,
            active.delivery_state
        );
    }

    #[test]
    fn missing_assessments_stay_missing() {
        let summary = aggregate(&[completed(), TodoGoal::default()]).unwrap();
        assert_eq!(summary.closed_feedback_loop, None);
        assert_eq!(summary.feedback_loop_relevance, None);
        assert_eq!(summary.feedback_loop_coverage, None);
        assert_eq!(summary.feedback_loop_traceability, None);
        assert_eq!(summary.delivery_state, None);
        assert_eq!(summary.autonomy, None);
        assert_eq!(summary.iteration_maturity, None);
        assert_eq!(summary.feedback_loop, None);
        assert_eq!(summary.stopping_evidence, None);
        assert_eq!(summary.difficulty, Some(Difficulty::Involved));
    }

    #[test]
    fn strictest_requirements_are_preserved() {
        let mut routine = completed();
        routine.difficulty = Some(Difficulty::Routine);
        routine.feedback_loop_coverage = Some(FeedbackLoopCoverage::MainPaths);
        routine.feedback_loop_relevance = Some(FeedbackLoopRelevance::Representative);
        routine.feedback_loop_traceability = Some(FeedbackLoopTraceability::Partial);
        let summary = aggregate(&[completed(), routine]).unwrap();
        assert_eq!(summary.difficulty, Some(Difficulty::Involved));
        assert!(!jcode_base::todo::feedback_loop_coverage_passes(&summary));
        assert!(!jcode_base::todo::feedback_loop_relevance_passes(&summary));
        assert!(!jcode_base::todo::feedback_loop_traceability_passes(
            &summary
        ));
    }

    #[test]
    fn impossibility_requires_every_lane_and_retains_reasons() {
        let mut first = completed();
        first.cannot_complete = Some("Dependency unavailable".into());
        assert!(
            aggregate(&[first.clone(), completed()])
                .unwrap()
                .cannot_complete
                .is_none()
        );
        let mut second = completed();
        second.group = Some("Other lane".into());
        second.cannot_complete = Some("Hardware unavailable".into());
        let summary = aggregate(&[first, second]).unwrap();
        assert_eq!(
            summary.cannot_complete.as_deref(),
            Some("Completed lane: Dependency unavailable\nOther lane: Hardware unavailable")
        );
    }

    #[test]
    fn differing_terminal_reasons_are_not_a_shared_outcome() {
        let mut other = completed();
        other.iteration_maturity = Some(IterationMaturity::BudgetExhausted);
        assert_eq!(
            aggregate(&[completed(), other]).unwrap().iteration_maturity,
            None
        );
    }
}

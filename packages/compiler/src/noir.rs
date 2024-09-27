use std::{collections::BTreeSet, fs::File, io::Write, path::Path};

use itertools::Itertools;

use crate::structs::RegexAndDFA;

const ACCEPT_STATE_ID: &str = "accept";

pub fn gen_noir_fn(
    regex_and_dfa: &RegexAndDFA,
    path: &Path,
    gen_substrs: bool,
) -> Result<(), std::io::Error> {
    let noir_fn = to_noir_fn(regex_and_dfa, gen_substrs);
    let mut file = File::create(path)?;
    file.write_all(noir_fn.as_bytes())?;
    file.flush()?;
    Ok(())
}

/// Generates Noir code based on the DFA and whether a substring should be extracted.
///
/// # Arguments
///
/// * `regex_and_dfa` - The `RegexAndDFA` struct containing the regex pattern and DFA.
/// * `gen_substrs` - A boolean indicating whether to generate substrings.
///
/// # Returns
///
/// A `String` that contains the Noir code
fn to_noir_fn(regex_and_dfa: &RegexAndDFA, gen_substrs: bool) -> String {
    // Multiple accepting states are not supported
    // This is a vector nonetheless, to support an extra accepting state we'll use
    // to allow any character occurrences after the original accepting state
    let mut accept_state_ids: Vec<usize> = {
        let accept_states = regex_and_dfa
            .dfa
            .states
            .iter()
            .filter(|s| s.state_type == ACCEPT_STATE_ID)
            .map(|s| s.state_id)
            .collect_vec();
        assert!(
            accept_states.len() == 1,
            "there should be exactly 1 accept state"
        );
        accept_states
    };

    // Create the function that determines next state
    let mut next_state_fn_body = String::new();
    // Handle curr_state + char_code -> next_state
    let mut rows: Vec<(usize, u8, u8, usize)> = vec![];

    for state in regex_and_dfa.dfa.states.iter() {
        for (&tran_next_state_id, tran) in &state.transitions {
            let mut sorted_chars: Vec<&u8> = tran.iter().collect();
            sorted_chars.sort();

            let mut current_range_start: Option<u8> = None;
            let mut previous_char: Option<u8> = None;

            for &char_code in sorted_chars {
                if let Some(prev) = previous_char {
                    if char_code == prev + 1 {
                        // Extend the range if consecutive
                        previous_char = Some(char_code);
                    } else {
                        // Push the completed range or single character
                        rows.push((
                            state.state_id,
                            current_range_start.unwrap(),
                            prev,
                            tran_next_state_id,
                        ));
                        // Start a new range
                        current_range_start = Some(char_code);
                        previous_char = Some(char_code);
                    }
                } else {
                    // First character in the range
                    current_range_start = Some(char_code);
                    previous_char = Some(char_code);
                }
            }

            // Push the last range or single character
            if let Some(start) = current_range_start {
                rows.push((
                    state.state_id,
                    start,
                    previous_char.unwrap(),
                    tran_next_state_id,
                ));
            }
        }
    }

    // The first transitions that happen out of state 0
    // disregarding the transition to 255
    let first_transitions: Vec<(u8, u8, usize)> = rows
        .clone()
        .into_iter()
        .filter(|(curr_state, char_start, _, _)| *curr_state == 0 && *char_start != 255)
        .map(|(_, start, end, next)| (start, end, next))
        .collect();
    let mut first_condition: bool = true;

    // The reset boolean is needed when generating substrings
    // It is set to false, unless matching the regex fails and the state is reset (to 0 or 1)
    let reset_flip_if_needed = if gen_substrs {
        "\n   reset = false;"
    } else {
        ""
    };

    // Add all rows to next state function
    for (curr_state_id, start_char_code, end_char_code, next_state_id) in rows.clone().iter() {
        if first_condition {
            if start_char_code == end_char_code {
                next_state_fn_body += &format!(
                  "if (s == {curr_state_id}) & (input == {start_char_code}) {{\n   next = {next_state_id};{reset_flip_if_needed}\n}}"
              );
            } else {
                next_state_fn_body += &format!(
                  "if (s == {curr_state_id}) & (input >= {start_char_code}) & (input <= {end_char_code}) {{\n   next = {next_state_id};{reset_flip_if_needed}\n}}"
              );
            }
            first_condition = false;
        } else {
            if start_char_code == end_char_code {
                next_state_fn_body += &format!(
                  " else if (s == {curr_state_id}) & (input == {start_char_code}) {{\n   next = {next_state_id};{reset_flip_if_needed}\n}}"
              );
            } else {
                next_state_fn_body += &format!(
                  " else if (s == {curr_state_id}) & (input >= {start_char_code}) & (input <= {end_char_code}) {{\n   next = {next_state_id};{reset_flip_if_needed}\n}}"
              );
            }
        }
    }

    // In case that there is no end_anchor, we add an additional accepting state to which any
    // character occurence after the accepting state will go.
    // This needs to be a new state, otherwise substring extraction won't work correctly
    if !regex_and_dfa.has_end_anchor {
        let original_accept_id = accept_state_ids.get(0).unwrap().clone();
        // Create a new highest state
        let extra_accept_id = regex_and_dfa
            .dfa
            .states
            .iter()
            .max_by_key(|state| state.state_id)
            .map(|state| state.state_id)
            .unwrap()
            + 1;
        accept_state_ids.push(extra_accept_id);
        if first_condition {
            next_state_fn_body +=
                &format!("if (s == {original_accept_id}) {{\n   next = {extra_accept_id};{reset_flip_if_needed}\n}}");
        } else {
            next_state_fn_body += &format!(
                " else if (s == {original_accept_id}) {{\n   next = {extra_accept_id};{reset_flip_if_needed}\n}}"
            );
        }
        // And when that accepting state is encountered, stay in it.
        next_state_fn_body +=
            &format!(" else if (s == {extra_accept_id}) {{\n   next = {extra_accept_id};{reset_flip_if_needed}\n}}");
    }

    // Add the restart for transitions out of 0, if nothing else has matched
    // this is needed for a "restart"
    for (char_start, char_end, next_state) in first_transitions {
        if char_start == char_end {
            next_state_fn_body +=
                &format!(" else if (input == {char_start}) {{\n   next = {next_state};\n}}");
        } else {
            next_state_fn_body += &format!(
          " else if ((input >= {char_start}) & (input <= {char_end})) {{\n   next = {next_state};\n}}");
        }
    }

    next_state_fn_body = indent(&next_state_fn_body, 1);
    let next_state_fn = if gen_substrs {
        format!(
            r#"
struct StateInfo {{
  next_state: Field,
  reset: bool
      }}

fn next_state(s: Field, input: u8) -> StateInfo {{
    let mut next = 0;
    // Whether the move to the next state can be seen as a reset
    // and what has been observed until now turned out to be invalid
    let mut reset = true;
{next_state_fn_body}

    StateInfo{{ next_state: next, reset }}
}}
  "#
        )
    } else {
        format!(
            r#"
fn next_state(s: Field, input: u8) -> Field {{
    let mut next = 0;
{next_state_fn_body}

    next
}}
  "#
        )
    };

    // Add check whether the final state is an accepting state
    // From the DFA we should get a unique accepting state, but there can be
    // an additional accepting state we created, if the regex doesn't end with $
    let final_states_condition_body = accept_state_ids
        .iter()
        .map(|id| format!("(s == {id})"))
        .collect_vec()
        .join(" | ");

    // substring_ranges contains the transitions that belong to the substring
    let substr_ranges: &Vec<BTreeSet<(usize, usize)>> = &regex_and_dfa.substrings.substring_ranges;
    // Note: substring_boundaries is only filled if the substring info is coming from decomposed setting
    //  and will be empty in the raw setting (using json file for substr transitions). This is why substring_ranges is used here

    let fn_body = if gen_substrs {
        let mut first_condition = true;

        let mut conditions = substr_ranges
            .iter()
            .enumerate()
            .map(|(set_idx, range_set)| {
                // Combine the range conditions into a single line using `|` operator
                let range_conditions = range_set
                    .iter()
                    .map(|(range_start, range_end)| {
                        format!("((s == {range_start}) & (s_next == {range_end}))")
                    })
                    .collect::<Vec<_>>()
                    .join(" | ");

                // For the first condition, use `if`, for others, use `else if`
                let start_part = if first_condition {
                    first_condition = false;
                    "if"
                } else {
                    "else if"
                };

                // The body of the condition handling substring creation/updating
                format!(
                    "{start_part} ({range_conditions}) {{
    if (consecutive_substr == 0) {{
      current_substring.push(temp);
      consecutive_substr = 1;
    }} else if (consecutive_substr == 1) {{
      current_substring.push(temp);
    }}
}}"
                )
            })
            .collect::<Vec<_>>()
            .join("\n");

        // If a substring was in the making but there's a jump back to the start, remove it
        // the substring was not valid

        // Add the final else if for resetting the consecutive_substr
        let final_conditions = format!(
            "{conditions} else if ((consecutive_substr == 1) & (s_next == 0)) {{
    current_substring = BoundedVec::new();
    consecutive_substr = 0;
}} else if (consecutive_substr == 1) {{
    // The substring is done so \"save\" it
    substrings.push(current_substring);
    // reset the substring holder for next use
    current_substring = BoundedVec::new();
    consecutive_substr = 0;
}}"
        );

        conditions = indent(&final_conditions, 2); // Indent twice to align with the for loop's body

        format!(
            r#"
pub fn regex_match<let N: u32>(input: [u8; N]) -> Vec<BoundedVec<Field, N>> {{
    // regex: {regex_pattern}
    let mut substrings: Vec<BoundedVec<Field, N>> = Vec::new();

    // "Previous" state
    let mut s: Field = 0;
    // "Next"/upcoming state
    let mut s_next: Field = 0;
    let stateInfo = next_state(s, 255);
    s = stateInfo.next_state;
    let mut consecutive_substr = 0;
    let mut current_substring = BoundedVec::new();

    for i in 0..input.len() {{
        let stateInfo =  next_state(s, input[i]);
        if stateInfo.reset {{
            // If there was a reset, we consider the previous state to be 0.
            s = 0;
        }}
        s_next = stateInfo.next_state;
        let temp = input[i] as Field;

        // If a substring was in the making, but the state was reset
        // we disregard previous progress because apparently it is invalid
        if (stateInfo.reset & (consecutive_substr == 1)) {{
            current_substring = BoundedVec::new();
            consecutive_substr = 0;
        }}

        // Fill up substrings
{conditions}
        s = s_next;
    }}

    assert({final_states_condition_body}, f"no match: {{s}}");

    // Add pending substring that hasn't been added
    if consecutive_substr == 1 {{
      substrings.push(current_substring);
    }}
    substrings
}}
  "#,
            regex_pattern = regex_and_dfa.regex_pattern,
        )
    } else {
        format!(
            r#"
pub fn regex_match<let N: u32>(input: [u8; N]) {{
    // regex: {regex_pattern}
    let mut s = 0;
    s = next_state(s, 255);

    for i in 0..input.len() {{
        s = next_state(s, input[i]);
    }}

    assert({final_states_condition_body}, f"no match: {{s}}");
}}
  "#,
            regex_pattern = regex_and_dfa.regex_pattern,
        )
    };

    format!(
        r#"
      {fn_body}
      {next_state_fn}
  "#
    )
    .trim()
    .to_owned()
}

/// Indents each line of the given string by a specified number of levels.
/// Each level adds four spaces to the beginning of non-whitespace lines.
fn indent(s: &str, level: usize) -> String {
    let indent_str = "    ".repeat(level);
    s.split("\n")
        .map(|s| {
            if s.trim().is_empty() {
                s.to_owned()
            } else {
                format!("{}{}", indent_str, s)
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

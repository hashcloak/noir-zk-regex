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

    let mut first_condition: Option<(u8, u8, usize)> = None;

    // Add all rows to next state function
    for (curr_state_id, start_char_code, end_char_code, next_state_id) in rows.iter() {
        if first_condition.is_none() {
            if start_char_code == end_char_code {
                next_state_fn_body += &format!(
                  "if (s == {curr_state_id}) & (input == {start_char_code}) {{\n   next = {next_state_id};\n}}"
              );
            } else {
                next_state_fn_body += &format!(
                  "if (s == {curr_state_id}) & (input >= {start_char_code}) & (input <= {end_char_code}) {{\n   next = {next_state_id};\n}}"
              );
            }
            first_condition = Some((*start_char_code, *end_char_code, *next_state_id));
        } else {
            if start_char_code == end_char_code {
                next_state_fn_body += &format!(
                  " else if (s == {curr_state_id}) & (input == {start_char_code}) {{\n   next = {next_state_id};\n}}"
              );
            } else {
                next_state_fn_body += &format!(
                  " else if (s == {curr_state_id}) & (input >= {start_char_code}) & (input <= {end_char_code}) {{\n   next = {next_state_id};\n}}"
              );
            }
        }
    }

    // In case that there is no end_anchor, we add an additional accepting state to which any
    // character occurence after the accepting state will go.
    // This needs to be a new state, otherwise substring extraction won't work correctly
    if !regex_and_dfa.has_end_anchor {
        let original_accept_id = accept_state_ids.get(0).unwrap().clone();
        let extra_accept_id = original_accept_id + 1;
        accept_state_ids.push(extra_accept_id);
        if first_condition.is_none() {
            next_state_fn_body +=
                &format!("if (s == {original_accept_id}) {{\n  next = {extra_accept_id};\n}}");
        } else {
            next_state_fn_body += &format!(
                " else if (s == {original_accept_id}) {{\n   next = {extra_accept_id};\n}}"
            );
        }
        // And when that accepting state is encountered, stay in it.
        next_state_fn_body += &format!(
          " else if (s == {extra_accept_id}) {{\n   next = {extra_accept_id};\n}}"
      );
    }

    // Add the restart for the first state transition, if nothing else has matched
    // this is needed for a "restart"
    if first_condition.is_some() {
        let (char_start, char_end, next_state) = first_condition.unwrap();
        // if the first transition is for 255, that is the indication of the beginning of the string
        // for caret anchor support. So adding this transition is not needed
        if char_start != 255 {
            if char_start == char_end {
                next_state_fn_body +=
                    &format!(" else if (input == {char_start}) {{\n next = {next_state};\n}}");
            } else {
                next_state_fn_body += &format!(
          " else if ((input >= {char_start}) & (input <= {char_end})) {{\n next = {next_state};\n}}"
      );
            }
        }
    }

    next_state_fn_body = indent(&next_state_fn_body, 1);
    let next_state_fn = format!(
        r#"
fn next_state(s: Field, input: u8) -> Field {{
    let mut next = 0;
{next_state_fn_body}

    next
}}
  "#
    );

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
                        format!("(s == {range_start}) & (s_next == {range_end})")
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
      let mut substr{set_idx} = BoundedVec::new();
      substr{set_idx}.push(temp);
      substrings.push(substr{set_idx});
      consecutive_substr = 1;
      substr_count += 1;
    }} else if (consecutive_substr == 1) {{
      let mut current: BoundedVec<Field, N> = substrings.get(substr_count - 1);
      current.push(temp);
      substrings.set(substr_count - 1, current);
    }}
}}"
                )
            })
            .collect::<Vec<_>>()
            .join("\n");

        // Add the final else if for resetting the consecutive_substr
        let final_conditions = format!(
            "{conditions} else if (consecutive_substr == 1) {{
    consecutive_substr = 0;
}}"
        );

        conditions = indent(&final_conditions, 2); // Indent twice to align with the for loop's body

        format!(
            r#"
pub fn regex_match<let N: u32>(input: [u8; N]) -> Vec<BoundedVec<Field, N>> {{
    // regex: {regex_pattern}
    let mut substrings: Vec<BoundedVec<Field, N>> = Vec::new();
    // Workaround for pop bug with Vec
    let mut substr_count = 0;

    // "Previous" state
    let mut s: Field = 0;
    // "Next"/upcoming state
    let mut s_next: Field = 0;
    s = next_state(s, 255);
    let mut consecutive_substr = 0;

    for i in 0..input.len() {{
        s_next = next_state(s, input[i]);
        let temp = input[i] as Field;
        // Fill up substrings
{conditions}
        s = s_next;
    }}

    assert({final_states_condition_body}, f"no match: {{s}}");
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

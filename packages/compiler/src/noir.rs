use std::{fs::File, io::Write, path::Path};

use itertools::Itertools;

use crate::structs::RegexAndDFA;

const ACCEPT_STATE_ID: &str = "accept";

pub fn gen_noir_fn(regex_and_dfa: &RegexAndDFA, path: &Path) -> Result<(), std::io::Error> {
    let noir_fn = to_noir_fn(regex_and_dfa);
    let mut file = File::create(path)?;
    file.write_all(noir_fn.as_bytes())?;
    file.flush()?;
    Ok(())
}

fn to_noir_fn(regex_and_dfa: &RegexAndDFA) -> String {
    // Multiple accepting states are not supported
    let accept_state_id = {
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
        *accept_states.get(0).unwrap()
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
                  "if (s == {curr_state_id}) & (input == {start_char_code}) {{\n next = {next_state_id};\n}}"
              );
            } else {
                next_state_fn_body += &format!(
                  "if (s == {curr_state_id}) & (input >= {start_char_code}) & (input <= {end_char_code}) {{\n next = {next_state_id};\n}}"
              );
            }
            first_condition = Some((*start_char_code, *end_char_code, *next_state_id));
        } else {
            if start_char_code == end_char_code {
                next_state_fn_body += &format!(
                  " else if (s == {curr_state_id}) & (input == {start_char_code}) {{\n next = {next_state_id};\n}}"
              );
            } else {
                next_state_fn_body += &format!(
                  " else if (s == {curr_state_id}) & (input >= {start_char_code}) & (input <= {end_char_code}) {{\n next = {next_state_id};\n}}"
              );
            }
        }
    }

    // Add accepting state logic
    if !regex_and_dfa.has_end_anchor {
        if first_condition.is_none() {
            next_state_fn_body +=
                &format!("if (s == {accept_state_id}) {{\n  next = {accept_state_id};\n}}");
        } else {
            next_state_fn_body +=
                &format!(" else if (s == {accept_state_id}) {{\n  next = {accept_state_id};\n}}");
        }
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

    next_state_fn_body = indent(&next_state_fn_body);
    let next_state_fn = format!(
        r#"
fn next_state(s: Field, input: u8) -> Field {{
    let mut next = 0;
{next_state_fn_body}

    next
}}
  "#
    );

    let fn_body = format!(
        r#"
pub fn regex_match<let N: u32>(input: [u8; N]) {{
    // regex: {regex_pattern}
    let mut s = 0;
    s = next_state(s, 255);

    for i in 0..input.len() {{
        s = next_state(s, input[i]);
    }}

    assert(s == {accept_state_id}, f"no match: {{s}}");
}}
  "#,
        regex_pattern = regex_and_dfa.regex_pattern,
    );
    format!(
        r#"
      {fn_body}
      {next_state_fn}
  "#
    )
    .trim()
    .to_owned()
}

fn indent(s: &str) -> String {
    s.split("\n")
        .map(|s| {
            if s.trim().is_empty() {
                s.to_owned()
            } else {
                format!("{}{}", "    ", s)
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

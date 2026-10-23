---------------------------- MODULE ClockLifecycle ----------------------------
EXTENDS Integers, FiniteSets, Sequences, TLC
CONSTANTS Workload, Rule, Cap, FiniteTimeout
ASSUME /\ Workload \in {"Advances", "ResumePause", "Inhibit", "Nested", "Mixed"}
       /\ Rule \in {"Original", "Conditional", "NoPaused", "NoInhibit", "StaleRelative"}
       /\ Cap \in {1, 2, 3}
       /\ FiniteTimeout \in BOOLEAN
VARIABLE p
vars == <<p>>
Deadlines == {2, 4}
LastDeadline == 4
Program == CASE Workload = "Advances" -> <<"Advance", "Advance">>
                [] Workload = "ResumePause" -> <<"Resume", "Pause">>
                [] Workload = "Inhibit" -> <<"Register", "Release">>
                [] Workload = "Nested" -> <<"Register", "Register", "Advance", "Release", "Release", "Advance">>
                [] OTHER -> <<"Register", "Advance", "Release", "Advance">>
ExternalDelta == Cardinality({i \in 1..Len(Program) : Program[i] = "Advance"})
MaxTime == LastDeadline + ExternalDelta + LastDeadline
Max(a, b) == IF a > b THEN a ELSE b
W == INSTANCE WaitPolicy WITH RemoveFloor <- FALSE, sample <- p
State == IF p.mode = "Running" THEN "Running"
         ELSE IF p.count = 0 THEN "Paused" ELSE "Inhibited"
ControlsDone == p.index > Len(Program)
TimersDone == p.delivered = Deadlines
Init == p = [mode |-> "Paused", count |-> 0, now |-> 0,
    index |-> 1, caller |-> "Mutate", pc |-> "Read", saved |-> "Paused",
    sampledNow |-> 0, duration |-> 0, effective |-> 0, target |-> 0,
    flag |-> FALSE, token |-> FALSE, delivered |-> {},
    panicked |-> FALSE, badEligibility |-> FALSE, overshot |-> FALSE,
    stale |-> FALSE, registeredAfterSnapshot |-> FALSE,
    externalAdvances |-> 0, repeated |-> FALSE, skipped |-> FALSE]
Operation == IF ControlsDone THEN "Done" ELSE Program[p.index]
Mutate ==
    /\ ~ControlsDone /\ p.caller = "Mutate"
    /\ (Operation # "Resume" \/ p.mode = "Paused")
    /\ (Operation # "Pause" \/ p.mode = "Running")
    /\ (Operation # "Advance" \/ p.mode = "Paused")
    /\ (Operation # "Release" \/ p.count > 0)
    /\ p' = [p EXCEPT
        !.mode = CASE Operation = "Resume" -> "Running"
                      [] Operation = "Pause" -> "Paused" [] OTHER -> @,
        !.count = CASE Operation = "Register" -> @ + 1
                       [] Operation = "Release" -> @ - 1 [] OTHER -> @,
        !.now = IF Operation = "Advance" THEN @ + 1 ELSE @,
        !.caller = IF Operation \in {"Register", "Pause"} THEN "Done" ELSE "Flag",
        !.registeredAfterSnapshot = @ \/ (Operation = "Register" /\ p.pc = "Auto"),
        !.externalAdvances = IF Operation = "Advance" THEN @ + 1 ELSE @,
        !.repeated = @ \/ (Operation = "Advance" /\ p.externalAdvances > 0)]
Flag == /\ p.caller = "Flag"
        /\ p' = [p EXCEPT !.flag = TRUE, !.caller = "Token"]
Token == /\ p.caller = "Token"
         /\ p' = [p EXCEPT !.token = TRUE, !.caller = "Done"]
Finish == /\ p.caller = "Done" /\ ~ControlsDone
          /\ p' = [p EXCEPT !.index = @ + 1, !.caller = "Mutate"]
Caller == Mutate \/ Flag \/ Token \/ Finish
Read ==
    /\ p.pc = "Read" /\ ~p.panicked /\ ~(ControlsDone /\ TimersDone)
    /\ LET pending == Deadlines \ p.delivered
           deadline == IF pending = {} THEN p.now ELSE CHOOSE d \in pending : \A e \in pending : d <= e
       IN p' = [p EXCEPT !.sampledNow = p.now,
            !.duration = Max(0, deadline - p.now),
            !.pc = IF deadline <= p.now THEN "Process" ELSE "Snapshot"]
Snapshot ==
    /\ p.pc = "Snapshot"
    /\ LET effective == W!Policy(State, p.duration, Cap)
       IN p' = [p EXCEPT !.saved = State, !.effective = effective,
                        !.target = p.sampledNow + effective, !.pc = "Poll"]
Poll == /\ p.pc = "Poll"
        /\ p' = [p EXCEPT !.token = FALSE,
            !.pc = IF p.saved = "Paused" THEN "FlagCheck" ELSE IF p.token THEN "Process" ELSE "Wait"]
WaitReturn == /\ p.pc = "Wait" /\ p.token
              /\ p' = [p EXCEPT !.token = FALSE, !.pc = "Process"]
TimeoutReturn == /\ FiniteTimeout /\ p.pc = "Wait"
                 /\ p.saved \in {"Running", "Inhibited"}
                 /\ p' = [p EXCEPT !.token = FALSE, !.pc = "Process"]
FlagCheck == /\ p.pc = "FlagCheck"
             /\ p' = [p EXCEPT !.flag = FALSE, !.pc = IF p.flag THEN "Process" ELSE "Auto"]
Auto ==
    /\ p.pc = "Auto"
    /\ LET eligible == p.mode = "Paused" /\ p.count = 0
           fresh == p.now = p.sampledNow
           commit == CASE Rule = "Original" -> p.mode = "Paused"
                          [] Rule = "NoPaused" -> p.count = 0 /\ fresh
                          [] Rule = "NoInhibit" -> p.mode = "Paused" /\ fresh
                          [] Rule = "StaleRelative" -> eligible
                          [] OTHER -> eligible /\ fresh
           nextNow == IF commit THEN p.now + p.effective ELSE p.now
       IN p' = [p EXCEPT !.now = nextNow, !.pc = "Process",
            !.panicked = @ \/ (Rule = "Original" /\ p.mode = "Running"),
            !.badEligibility = @ \/ (commit /\ ~eligible),
            !.overshot = @ \/ (commit /\ nextNow > Max(p.now, p.target)),
            !.stale = @ \/ ~fresh, !.skipped = @ \/ ~commit]
Process == /\ p.pc = "Process" /\ ~p.panicked
           /\ p' = [p EXCEPT !.delivered = @ \cup {d \in Deadlines : d <= p.now}, !.pc = "Read"]
Driver == Read \/ Snapshot \/ Poll \/ WaitReturn \/ TimeoutReturn \/ FlagCheck \/ Auto \/ Process
Next == Caller \/ Driver
Spec == Init /\ [][Next]_vars /\ WF_vars(Caller) /\ WF_vars(Driver)
TypeOK == /\ p.mode \in {"Paused", "Running"} /\ p.count \in 0..2
          /\ p.now \in 0..MaxTime /\ p.index \in 1..(Len(Program) + 1)
          /\ p.delivered \subseteq Deadlines
          /\ p.pc \in {"Read", "Snapshot", "Poll", "Wait", "FlagCheck", "Auto", "Process"}
          /\ p.caller \in {"Mutate", "Flag", "Token", "Done"}
          /\ p.saved \in {"Running", "Paused", "Inhibited"}
          /\ p.sampledNow \in 0..MaxTime /\ p.target \in 0..MaxTime
          /\ p.duration \in 0..LastDeadline /\ p.effective \in 0..LastDeadline
          /\ p.externalAdvances \in 0..ExternalDelta
          /\ \A field \in {"flag", "token", "panicked", "badEligibility", "overshot", "stale",
                             "registeredAfterSnapshot", "repeated", "skipped"} : p[field] \in BOOLEAN
TimerNotEarly == \A d \in p.delivered : p.now >= d
ClockMonotonic == [][p'.now >= p.now]_vars
AutoEligible == ~p.badEligibility
AutoWithinTarget == ~p.overshot
NoPanic == ~p.panicked
Completion == <> (ControlsDone /\ TimersDone)
WitnessRepeated == ~p.repeated
WitnessRegistration == ~p.registeredAfterSnapshot
WitnessStale == ~p.stale
WitnessTimers == ~TimersDone
WitnessSkipped == ~p.skipped
=============================================================================

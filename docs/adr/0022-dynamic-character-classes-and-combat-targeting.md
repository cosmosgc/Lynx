# ADR 0022: Dynamic Character Classes and Combat Targeting

## Status
Proposed | To be reviewed by Core Team (Combat, AI, Persistence)

## Goal
To evolve character roles from static, pre-determined archetypes to dynamic, emergent classes defined at runtime by the synergy of equipped items, learned skills, and active features. This fundamentally changes how player combat interactions are perceived and implemented, making class utility flexible rather than restrictive.

## Problem Statement
Current systems rely on hardcoded or simple character roles. To create a modern, engaging MMORPG experience, we must implement a nuanced system where a character's "class" is an emergent property of their current build, not merely a fixed choice at creation. Furthermore, combat targeting needs to be more sophisticated than simply prioritizing the lowest health target; it must prioritize targets based on tactical utility and threat generation (e.g., tanking).

## Proposed Solution: Dynamic Class Assignment via 'Treat'
The system will introduce several interdependent components:

### 1. Emergent Class Definition
A character's class is determined by analyzing a combination of data points, including:
*   **Equipped Items:** Specific items grant innate class affinities or boosts (e.g., "Plate Armor" leans towards Tank; "Staves" lean towards Support).
*   **Skills/Features:** The highest concentration of learned skills in certain domains defines the primary role (e.g., high damage output spells point to DPS).

The system must calculate a weighted score across defined archetypes (Tank, Support, Damage, etc.) and assign the character's *dominant class* dynamically. This assignment should be visible on the character sheet but not immutable.

### 2. The Combat 'Treat' Mechanism
We introduce a core mechanic called **"Treat,"** which represents a character's current tactical presence or threat generation value in combat, similar to World of Warcraft mechanics.

*   **Tanks (Primary Function):** Tanks are designed to generate "Treat" with minimal effort relative to the damage they absorb or control. Their primary function is generating this high-value 'Treat' score, making them the highest priority target for enemy AI.
    *   *Mechanism:* When a Tank engages combat, their actions should prioritize increasing their measurable threat value over simply mitigating raw damage.
*   **Support Classes:** Support characters must generate and manipulate 'Treat' by empowering allies or debuffing enemies. Their contribution to team survivability is measured via this score.
    *   *Example:* Applying a critical buff might grant an ally bonus Treat, drawing enemy attention away from the main threat.
*   **Damage (DPS) Classes:** DPS characters contribute to 'Treat' primarily through raw output and reliable damage spikes. Their contribution must be weighted by their ability to apply status effects or force target priority.

### 3. NPC Combat AI Integration (The Targeting Rule)
The combat simulation engine must overhaul its targeting logic:

*   **Target Selection:** In any multi-target engagement, the enemy group must select the single highest "Treat" character as the primary threat/target. This ensures that tanks are always prioritized and validates the tanking role.
*   **Damage Scaling/Modification:** Damage output from classes must be modified by their assigned class:
    *   **DPS:** Damage dealt is calculated normally, but scaling factors should apply bonuses when targeting high-'Treat' enemies (due to enemy focus).
    *   **Support:** Utility functions (healing, buffs) are heavily weighted by the current Treat levels of allies.

## Implementation Dependencies and Next Steps
1.  **Combat Core Update:** The `WorldServer` must be updated to calculate and maintain a dynamic `Treat` value for all combatants in real-time.
2.  **AI Overhaul:** NPC pathing/combat logic must replace simple threat calculation with the 'Treat'-based targeting system described above.
3.  **Character System Refactor:** The character sheet/skill system needs to expose item and skill data that can be consumed by a central `ClassAssignmentEngine` service.

## Conclusion
This shift requires deep architectural changes across the Combat, AI, and Character systems, but it is critical for achieving a modern, engaging RPG feel where player agency dictates capability rather than pre-selected labels.
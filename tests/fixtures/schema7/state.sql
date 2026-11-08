PRAGMA foreign_keys=OFF;
BEGIN TRANSACTION;
CREATE TABLE agent_session_observations (
    session_socket_id TEXT NOT NULL,
    worktree_id TEXT NOT NULL,
    observation_id TEXT NOT NULL,
    address_id TEXT NOT NULL,
    agent_session_id TEXT NOT NULL,
    status TEXT NOT NULL,
    observed_at_ms INTEGER NOT NULL,
    PRIMARY KEY (session_socket_id, worktree_id, observation_id),
    FOREIGN KEY (session_socket_id, worktree_id, address_id)
        REFERENCES pane_addresses (session_socket_id, worktree_id, address_id)
);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794160809313905108-2019619-4-pane-address-1794160809313291547-2019619-1','pane-address-1794160809313291547-2019619-1','','unknown',1794160809312);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794160809335008085-2019619-5-pane-address-1794160809313291547-2019619-1','pane-address-1794160809313291547-2019619-1','','unknown',1794160809312);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794160809453002968-2019687-1-pane-address-1794160809313291547-2019619-1','pane-address-1794160809313291547-2019619-1','','unknown',1794160809433);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794160809517594213-2019766-4-pane-address-1794160809516957178-2019766-1','pane-address-1794160809516957178-2019766-1','','unknown',1794160809515);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794160809532013893-2019766-5-pane-address-1794160809313291547-2019619-1','pane-address-1794160809313291547-2019619-1','','unknown',1794160809515);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794160809532460150-2019766-6-pane-address-1794160809516957178-2019766-1','pane-address-1794160809516957178-2019766-1','','unknown',1794160809515);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794160809691048973-2019878-1-pane-address-1794160809313291547-2019619-1','pane-address-1794160809313291547-2019619-1','','unknown',1794160809679);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794160809691863250-2019878-2-pane-address-1794160809516957178-2019766-1','pane-address-1794160809516957178-2019766-1','','unknown',1794160809679);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794160809725535586-2019909-4-pane-address-1794160809724966979-2019909-1','pane-address-1794160809724966979-2019909-1','','unknown',1794160809723);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794160809740331050-2019909-5-pane-address-1794160809313291547-2019619-1','pane-address-1794160809313291547-2019619-1','','unknown',1794160809723);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794160809740732884-2019909-6-pane-address-1794160809516957178-2019766-1','pane-address-1794160809516957178-2019766-1','','unknown',1794160809723);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794160809740902462-2019909-7-pane-address-1794160809724966979-2019909-1','pane-address-1794160809724966979-2019909-1','','unknown',1794160809723);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794160809865981895-2019981-1-pane-address-1794160809313291547-2019619-1','pane-address-1794160809313291547-2019619-1','','unknown',1794160809854);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794160809866765305-2019981-2-pane-address-1794160809516957178-2019766-1','pane-address-1794160809516957178-2019766-1','','unknown',1794160809854);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794160809866967805-2019981-3-pane-address-1794160809724966979-2019909-1','pane-address-1794160809724966979-2019909-1','','unknown',1794160809854);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794160810375644335-2020121-1-pane-address-1794160809313291547-2019619-1','pane-address-1794160809313291547-2019619-1','','unknown',1794160810363);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794160810376418016-2020121-2-pane-address-1794160809516957178-2019766-1','pane-address-1794160809516957178-2019766-1','','unknown',1794160810363);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794160810376569831-2020121-3-pane-address-1794160809724966979-2019909-1','pane-address-1794160809724966979-2019909-1','','unknown',1794160810363);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794160810880042692-2020304-1-pane-address-1794160809313291547-2019619-1','pane-address-1794160809313291547-2019619-1','','unknown',1794160810867);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794160810880783291-2020304-2-pane-address-1794160809516957178-2019766-1','pane-address-1794160809516957178-2019766-1','e603eff8-5290-4a97-af5c-fb53c6a9b89e','unknown',1794160810867);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794160810880940516-2020304-3-pane-address-1794160809724966979-2019909-1','pane-address-1794160809724966979-2019909-1','','unknown',1794160810867);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794160811004933383-2020381-1-pane-address-1794160809313291547-2019619-1','pane-address-1794160809313291547-2019619-1','','unknown',1794160810992);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794160811005854100-2020381-2-pane-address-1794160809516957178-2019766-1','pane-address-1794160809516957178-2019766-1','e603eff8-5290-4a97-af5c-fb53c6a9b89e','unknown',1794160810992);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794160811006051180-2020381-3-pane-address-1794160809724966979-2019909-1','pane-address-1794160809724966979-2019909-1','','unknown',1794160810992);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794160813690114477-2020831-1-pane-address-1794160809313291547-2019619-1','pane-address-1794160809313291547-2019619-1','','unknown',1794160813678);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794160813690928995-2020831-2-pane-address-1794160809516957178-2019766-1','pane-address-1794160809516957178-2019766-1','e603eff8-5290-4a97-af5c-fb53c6a9b89e','idle',1794160813678);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794160813691122077-2020831-3-pane-address-1794160809724966979-2019909-1','pane-address-1794160809724966979-2019909-1','','unknown',1794160813678);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794160813789324509-2020889-1-pane-address-1794160809313291547-2019619-1','pane-address-1794160809313291547-2019619-1','','unknown',1794160813777);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794160813790293166-2020889-2-pane-address-1794160809516957178-2019766-1','pane-address-1794160809516957178-2019766-1','e603eff8-5290-4a97-af5c-fb53c6a9b89e','idle',1794160813777);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794160813790545459-2020889-3-pane-address-1794160809724966979-2019909-1','pane-address-1794160809724966979-2019909-1','','unknown',1794160813777);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794160814195076319-2021030-1-pane-address-1794160809313291547-2019619-1','pane-address-1794160809313291547-2019619-1','','unknown',1794160814183);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794160814195939838-2021030-2-pane-address-1794160809516957178-2019766-1','pane-address-1794160809516957178-2019766-1','e603eff8-5290-4a97-af5c-fb53c6a9b89e','idle',1794160814183);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794160814196144883-2021030-3-pane-address-1794160809724966979-2019909-1','pane-address-1794160809724966979-2019909-1','','working',1794160814183);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794160814796041674-2021239-1-pane-address-1794160809313291547-2019619-1','pane-address-1794160809313291547-2019619-1','','unknown',1794160814784);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794160814796843909-2021239-2-pane-address-1794160809516957178-2019766-1','pane-address-1794160809516957178-2019766-1','e603eff8-5290-4a97-af5c-fb53c6a9b89e','idle',1794160814784);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794160814797033475-2021239-3-pane-address-1794160809724966979-2019909-1','pane-address-1794160809724966979-2019909-1','','idle',1794160814784);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794160814885411208-2021239-1-pane-address-1794160809313291547-2019619-1','pane-address-1794160809313291547-2019619-1','','unknown',1794160814873);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794160814885902760-2021239-2-pane-address-1794160809516957178-2019766-1','pane-address-1794160809516957178-2019766-1','e603eff8-5290-4a97-af5c-fb53c6a9b89e','idle',1794160814873);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794160814886116771-2021239-3-pane-address-1794160809724966979-2019909-1','pane-address-1794160809724966979-2019909-1','','idle',1794160814873);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794160814965787130-2021239-1-pane-address-1794160809313291547-2019619-1','pane-address-1794160809313291547-2019619-1','','unknown',1794160814953);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794160814966267150-2021239-2-pane-address-1794160809516957178-2019766-1','pane-address-1794160809516957178-2019766-1','e603eff8-5290-4a97-af5c-fb53c6a9b89e','idle',1794160814953);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794160814966508353-2021239-3-pane-address-1794160809724966979-2019909-1','pane-address-1794160809724966979-2019909-1','','idle',1794160814953);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794160815097751982-2021386-1-pane-address-1794160809313291547-2019619-1','pane-address-1794160809313291547-2019619-1','','unknown',1794160815086);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794160815098596917-2021386-2-pane-address-1794160809516957178-2019766-1','pane-address-1794160809516957178-2019766-1','e603eff8-5290-4a97-af5c-fb53c6a9b89e','idle',1794160815086);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794160815098854760-2021386-3-pane-address-1794160809724966979-2019909-1','pane-address-1794160809724966979-2019909-1','','unknown',1794160815086);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794160815602567012-2021489-1-pane-address-1794160809313291547-2019619-1','pane-address-1794160809313291547-2019619-1','','unknown',1794160815590);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794160815603382311-2021489-2-pane-address-1794160809516957178-2019766-1','pane-address-1794160809516957178-2019766-1','e603eff8-5290-4a97-af5c-fb53c6a9b89e','idle',1794160815590);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794160815603623654-2021489-3-pane-address-1794160809724966979-2019909-1','pane-address-1794160809724966979-2019909-1','','unknown',1794160815590);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794160815698017885-2021542-1-pane-address-1794160809313291547-2019619-1','pane-address-1794160809313291547-2019619-1','','unknown',1794160815686);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794160815698763343-2021542-2-pane-address-1794160809516957178-2019766-1','pane-address-1794160809516957178-2019766-1','e603eff8-5290-4a97-af5c-fb53c6a9b89e','idle',1794160815686);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794160815698976804-2021542-3-pane-address-1794160809724966979-2019909-1','pane-address-1794160809724966979-2019909-1','','unknown',1794160815686);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794160817849625279-2021929-1-pane-address-1794160809313291547-2019619-1','pane-address-1794160809313291547-2019619-1','','unknown',1794160817528);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794160817851292667-2021929-2-pane-address-1794160809516957178-2019766-1','pane-address-1794160809516957178-2019766-1','e603eff8-5290-4a97-af5c-fb53c6a9b89e','working',1794160817528);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794160817851743924-2021929-3-pane-address-1794160809724966979-2019909-1','pane-address-1794160809724966979-2019909-1','','unknown',1794160817528);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794160818712115017-2021929-1-pane-address-1794160809313291547-2019619-1','pane-address-1794160809313291547-2019619-1','','unknown',1794160818694);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794160818712968769-2021929-2-pane-address-1794160809516957178-2019766-1','pane-address-1794160809516957178-2019766-1','e603eff8-5290-4a97-af5c-fb53c6a9b89e','working',1794160818694);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794160818713380552-2021929-3-pane-address-1794160809724966979-2019909-1','pane-address-1794160809724966979-2019909-1','','unknown',1794160818694);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794160819042316153-2021929-1-pane-address-1794160809313291547-2019619-1','pane-address-1794160809313291547-2019619-1','','unknown',1794160819023);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794160819043107858-2021929-2-pane-address-1794160809516957178-2019766-1','pane-address-1794160809516957178-2019766-1','e603eff8-5290-4a97-af5c-fb53c6a9b89e','idle',1794160819023);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794160819043521364-2021929-3-pane-address-1794160809724966979-2019909-1','pane-address-1794160809724966979-2019909-1','','working',1794160819023);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794160819145629733-2021929-1-pane-address-1794160809313291547-2019619-1','pane-address-1794160809313291547-2019619-1','','unknown',1794160819133);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794160819146359743-2021929-2-pane-address-1794160809516957178-2019766-1','pane-address-1794160809516957178-2019766-1','e603eff8-5290-4a97-af5c-fb53c6a9b89e','idle',1794160819133);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794160819146861664-2021929-3-pane-address-1794160809724966979-2019909-1','pane-address-1794160809724966979-2019909-1','','working',1794160819133);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794160819238743759-2021929-1-pane-address-1794160809313291547-2019619-1','pane-address-1794160809313291547-2019619-1','','unknown',1794160819226);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794160819239410510-2021929-2-pane-address-1794160809516957178-2019766-1','pane-address-1794160809516957178-2019766-1','e603eff8-5290-4a97-af5c-fb53c6a9b89e','idle',1794160819226);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794160819239707978-2021929-3-pane-address-1794160809724966979-2019909-1','pane-address-1794160809724966979-2019909-1','','idle',1794160819226);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794160819516285699-2022404-1-pane-address-1794160809313291547-2019619-1','pane-address-1794160809313291547-2019619-1','','unknown',1794160819504);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794160819517282919-2022404-2-pane-address-1794160809516957178-2019766-1','pane-address-1794160809516957178-2019766-1','e603eff8-5290-4a97-af5c-fb53c6a9b89e','idle',1794160819504);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794160819517543589-2022404-3-pane-address-1794160809724966979-2019909-1','pane-address-1794160809724966979-2019909-1','','working',1794160819504);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794160826151575090-2023131-1-pane-address-1794160809313291547-2019619-1','pane-address-1794160809313291547-2019619-1','','unknown',1794160826139);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794160826152413684-2023131-2-pane-address-1794160809516957178-2019766-1','pane-address-1794160809516957178-2019766-1','e603eff8-5290-4a97-af5c-fb53c6a9b89e','idle',1794160826139);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794160826152622525-2023131-3-pane-address-1794160809724966979-2019909-1','pane-address-1794160809724966979-2019909-1','','idle',1794160826139);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794160826238209149-2023131-1-pane-address-1794160809313291547-2019619-1','pane-address-1794160809313291547-2019619-1','','unknown',1794160826226);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794160826238610492-2023131-2-pane-address-1794160809516957178-2019766-1','pane-address-1794160809516957178-2019766-1','e603eff8-5290-4a97-af5c-fb53c6a9b89e','idle',1794160826226);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794160826238789157-2023131-3-pane-address-1794160809724966979-2019909-1','pane-address-1794160809724966979-2019909-1','','idle',1794160826226);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794160826318127688-2023131-1-pane-address-1794160809313291547-2019619-1','pane-address-1794160809313291547-2019619-1','','unknown',1794160826306);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794160826318664305-2023131-2-pane-address-1794160809516957178-2019766-1','pane-address-1794160809516957178-2019766-1','e603eff8-5290-4a97-af5c-fb53c6a9b89e','idle',1794160826306);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794160826318842319-2023131-3-pane-address-1794160809724966979-2019909-1','pane-address-1794160809724966979-2019909-1','','idle',1794160826306);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794160826445313508-2023285-1-pane-address-1794160809313291547-2019619-1','pane-address-1794160809313291547-2019619-1','','unknown',1794160826433);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794160826446179753-2023285-2-pane-address-1794160809516957178-2019766-1','pane-address-1794160809516957178-2019766-1','e603eff8-5290-4a97-af5c-fb53c6a9b89e','idle',1794160826433);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794160826446401369-2023285-3-pane-address-1794160809724966979-2019909-1','pane-address-1794160809724966979-2019909-1','','unknown',1794160826433);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794160826953616472-2023393-1-pane-address-1794160809313291547-2019619-1','pane-address-1794160809313291547-2019619-1','','unknown',1794160826941);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794160826954503276-2023393-2-pane-address-1794160809516957178-2019766-1','pane-address-1794160809516957178-2019766-1','e603eff8-5290-4a97-af5c-fb53c6a9b89e','idle',1794160826941);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794160826954762943-2023393-3-pane-address-1794160809724966979-2019909-1','pane-address-1794160809724966979-2019909-1','','unknown',1794160826941);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794160827059117770-2023449-1-pane-address-1794160809313291547-2019619-1','pane-address-1794160809313291547-2019619-1','','unknown',1794160827041);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794160827060087980-2023449-2-pane-address-1794160809516957178-2019766-1','pane-address-1794160809516957178-2019766-1','e603eff8-5290-4a97-af5c-fb53c6a9b89e','idle',1794160827041);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794160827060411968-2023449-3-pane-address-1794160809724966979-2019909-1','pane-address-1794160809724966979-2019909-1','','unknown',1794160827041);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794160830425874821-2024058-1-pane-address-1794160809313291547-2019619-1','pane-address-1794160809313291547-2019619-1','','unknown',1794160830399);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794160830429502768-2024058-2-pane-address-1794160809516957178-2019766-1','pane-address-1794160809516957178-2019766-1','e603eff8-5290-4a97-af5c-fb53c6a9b89e','idle',1794160830399);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794160830430021471-2024058-3-pane-address-1794160809724966979-2019909-1','pane-address-1794160809724966979-2019909-1','01a1bc51-9c88-71e0-a2cb-6d4a6121e0bb','working',1794160830399);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794160831001807796-2024058-1-pane-address-1794160809313291547-2019619-1','pane-address-1794160809313291547-2019619-1','','unknown',1794160830978);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794160831002679561-2024058-2-pane-address-1794160809516957178-2019766-1','pane-address-1794160809516957178-2019766-1','e603eff8-5290-4a97-af5c-fb53c6a9b89e','idle',1794160830978);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794160831003235905-2024058-3-pane-address-1794160809724966979-2019909-1','pane-address-1794160809724966979-2019909-1','01a1bc51-9c88-71e0-a2cb-6d4a6121e0bb','idle',1794160830978);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794160831662709852-2024058-1-pane-address-1794160809313291547-2019619-1','pane-address-1794160809313291547-2019619-1','','unknown',1794160831644);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794160831664621358-2024058-2-pane-address-1794160809516957178-2019766-1','pane-address-1794160809516957178-2019766-1','e603eff8-5290-4a97-af5c-fb53c6a9b89e','idle',1794160831644);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794160831665194523-2024058-3-pane-address-1794160809724966979-2019909-1','pane-address-1794160809724966979-2019909-1','01a1bc51-9c88-71e0-a2cb-6d4a6121e0bb','idle',1794160831644);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794160834494169107-2025494-1-pane-address-1794160809313291547-2019619-1','pane-address-1794160809313291547-2019619-1','','unknown',1794160834482);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794160834495084154-2025494-2-pane-address-1794160809516957178-2019766-1','pane-address-1794160809516957178-2019766-1','e603eff8-5290-4a97-af5c-fb53c6a9b89e','working',1794160834482);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794160834495432838-2025494-3-pane-address-1794160809724966979-2019909-1','pane-address-1794160809724966979-2019909-1','01a1bc51-9c88-71e0-a2cb-6d4a6121e0bb','idle',1794160834482);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794160842612843216-2028017-1-pane-address-1794160809313291547-2019619-1','pane-address-1794160809313291547-2019619-1','','unknown',1794160842600);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794160842613760347-2028017-2-pane-address-1794160809516957178-2019766-1','pane-address-1794160809516957178-2019766-1','e603eff8-5290-4a97-af5c-fb53c6a9b89e','idle',1794160842600);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794160842614035664-2028017-3-pane-address-1794160809724966979-2019909-1','pane-address-1794160809724966979-2019909-1','01a1bc51-9c88-71e0-a2cb-6d4a6121e0bb','working',1794160842600);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794160842710730342-2028017-1-pane-address-1794160809313291547-2019619-1','pane-address-1794160809313291547-2019619-1','','unknown',1794160842699);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794160842711298228-2028017-2-pane-address-1794160809516957178-2019766-1','pane-address-1794160809516957178-2019766-1','e603eff8-5290-4a97-af5c-fb53c6a9b89e','idle',1794160842699);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794160842711477454-2028017-3-pane-address-1794160809724966979-2019909-1','pane-address-1794160809724966979-2019909-1','01a1bc51-9c88-71e0-a2cb-6d4a6121e0bb','working',1794160842699);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794160845314924476-2028411-1-pane-address-1794160809313291547-2019619-1','pane-address-1794160809313291547-2019619-1','','unknown',1794160845303);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794160845315533629-2028411-2-pane-address-1794160809516957178-2019766-1','pane-address-1794160809516957178-2019766-1','e603eff8-5290-4a97-af5c-fb53c6a9b89e','idle',1794160845303);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794160845315835325-2028411-3-pane-address-1794160809724966979-2019909-1','pane-address-1794160809724966979-2019909-1','01a1bc51-9c88-71e0-a2cb-6d4a6121e0bb','idle',1794160845303);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794160845408469418-2028411-1-pane-address-1794160809313291547-2019619-1','pane-address-1794160809313291547-2019619-1','','unknown',1794160845396);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794160845409020652-2028411-2-pane-address-1794160809516957178-2019766-1','pane-address-1794160809516957178-2019766-1','e603eff8-5290-4a97-af5c-fb53c6a9b89e','working',1794160845396);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794160845409278717-2028411-3-pane-address-1794160809724966979-2019909-1','pane-address-1794160809724966979-2019909-1','01a1bc51-9c88-71e0-a2cb-6d4a6121e0bb','idle',1794160845396);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794160845965710013-2028751-1-pane-address-1794160809313291547-2019619-1','pane-address-1794160809313291547-2019619-1','','unknown',1794160845954);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794160845966602467-2028751-2-pane-address-1794160809516957178-2019766-1','pane-address-1794160809516957178-2019766-1','e603eff8-5290-4a97-af5c-fb53c6a9b89e','idle',1794160845954);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794160845966825726-2028751-3-pane-address-1794160809724966979-2019909-1','pane-address-1794160809724966979-2019909-1','01a1bc51-9c88-71e0-a2cb-6d4a6121e0bb','idle',1794160845954);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794160848035842774-2029195-1-pane-address-1794160809313291547-2019619-1','pane-address-1794160809313291547-2019619-1','','unknown',1794160848024);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794160848036699762-2029195-2-pane-address-1794160809516957178-2019766-1','pane-address-1794160809516957178-2019766-1','e603eff8-5290-4a97-af5c-fb53c6a9b89e','idle',1794160848024);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794160848036971662-2029195-3-pane-address-1794160809724966979-2019909-1','pane-address-1794160809724966979-2019909-1','01a1bc51-9c88-71e0-a2cb-6d4a6121e0bb','working',1794160848024);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794160861318546914-2031541-1-pane-address-1794160809313291547-2019619-1','pane-address-1794160809313291547-2019619-1','','unknown',1794160861306);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794160861319502027-2031541-2-pane-address-1794160809516957178-2019766-1','pane-address-1794160809516957178-2019766-1','e603eff8-5290-4a97-af5c-fb53c6a9b89e','working',1794160861306);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794160861319851703-2031541-3-pane-address-1794160809724966979-2019909-1','pane-address-1794160809724966979-2019909-1','01a1bc51-9c88-71e0-a2cb-6d4a6121e0bb','working',1794160861306);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794160862231396608-2031779-1-pane-address-1794160809313291547-2019619-1','pane-address-1794160809313291547-2019619-1','','unknown',1794160862219);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794160862232491683-2031779-2-pane-address-1794160809516957178-2019766-1','pane-address-1794160809516957178-2019766-1','e603eff8-5290-4a97-af5c-fb53c6a9b89e','idle',1794160862219);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794160862232724029-2031779-3-pane-address-1794160809724966979-2019909-1','pane-address-1794160809724966979-2019909-1','01a1bc51-9c88-71e0-a2cb-6d4a6121e0bb','working',1794160862219);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794160862814226721-2031870-1-pane-address-1794160809313291547-2019619-1','pane-address-1794160809313291547-2019619-1','','unknown',1794160862802);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794160862815216298-2031870-2-pane-address-1794160809516957178-2019766-1','pane-address-1794160809516957178-2019766-1','e603eff8-5290-4a97-af5c-fb53c6a9b89e','idle',1794160862802);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794160862815502846-2031870-3-pane-address-1794160809724966979-2019909-1','pane-address-1794160809724966979-2019909-1','01a1bc51-9c88-71e0-a2cb-6d4a6121e0bb','idle',1794160862802);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794160864013669502-2032069-1-pane-address-1794160809313291547-2019619-1','pane-address-1794160809313291547-2019619-1','','unknown',1794160864001);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794160864014642868-2032069-2-pane-address-1794160809516957178-2019766-1','pane-address-1794160809516957178-2019766-1','e603eff8-5290-4a97-af5c-fb53c6a9b89e','idle',1794160864001);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794160864014866879-2032069-3-pane-address-1794160809724966979-2019909-1','pane-address-1794160809724966979-2019909-1','01a1bc51-9c88-71e0-a2cb-6d4a6121e0bb','working',1794160864001);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794160865828254186-2032479-1-pane-address-1794160809313291547-2019619-1','pane-address-1794160809313291547-2019619-1','','unknown',1794160865816);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794160865829203788-2032479-2-pane-address-1794160809516957178-2019766-1','pane-address-1794160809516957178-2019766-1','e603eff8-5290-4a97-af5c-fb53c6a9b89e','idle',1794160865816);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794160865829510043-2032479-3-pane-address-1794160809724966979-2019909-1','pane-address-1794160809724966979-2019909-1','01a1bc51-9c88-71e0-a2cb-6d4a6121e0bb','idle',1794160865816);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794160867948513645-2032964-1-pane-address-1794160809313291547-2019619-1','pane-address-1794160809313291547-2019619-1','','unknown',1794160867936);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794160867949484286-2032964-2-pane-address-1794160809516957178-2019766-1','pane-address-1794160809516957178-2019766-1','e603eff8-5290-4a97-af5c-fb53c6a9b89e','working',1794160867936);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794160867949704209-2032964-3-pane-address-1794160809724966979-2019909-1','pane-address-1794160809724966979-2019909-1','01a1bc51-9c88-71e0-a2cb-6d4a6121e0bb','idle',1794160867936);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794160871269447864-2033608-1-pane-address-1794160809313291547-2019619-1','pane-address-1794160809313291547-2019619-1','','unknown',1794160871257);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794160871270362401-2033608-2-pane-address-1794160809516957178-2019766-1','pane-address-1794160809516957178-2019766-1','e603eff8-5290-4a97-af5c-fb53c6a9b89e','idle',1794160871257);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794160871270751481-2033608-3-pane-address-1794160809724966979-2019909-1','pane-address-1794160809724966979-2019909-1','01a1bc51-9c88-71e0-a2cb-6d4a6121e0bb','idle',1794160871257);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794160872483940793-2033809-1-pane-address-1794160809313291547-2019619-1','pane-address-1794160809313291547-2019619-1','','unknown',1794160872472);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794160872484836134-2033809-2-pane-address-1794160809516957178-2019766-1','pane-address-1794160809516957178-2019766-1','e603eff8-5290-4a97-af5c-fb53c6a9b89e','working',1794160872472);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794160872485093046-2033809-3-pane-address-1794160809724966979-2019909-1','pane-address-1794160809724966979-2019909-1','01a1bc51-9c88-71e0-a2cb-6d4a6121e0bb','idle',1794160872472);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794160873389330166-2033952-1-pane-address-1794160809313291547-2019619-1','pane-address-1794160809313291547-2019619-1','','unknown',1794160873377);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794160873390149233-2033952-2-pane-address-1794160809516957178-2019766-1','pane-address-1794160809516957178-2019766-1','e603eff8-5290-4a97-af5c-fb53c6a9b89e','idle',1794160873377);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794160873390369136-2033952-3-pane-address-1794160809724966979-2019909-1','pane-address-1794160809724966979-2019909-1','01a1bc51-9c88-71e0-a2cb-6d4a6121e0bb','idle',1794160873377);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794161388106941174-2122075-1-pane-address-1794160809313291547-2019619-1','pane-address-1794160809313291547-2019619-1','','unknown',1794161388095);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794161388108050571-2122075-2-pane-address-1794160809516957178-2019766-1','pane-address-1794160809516957178-2019766-1','e603eff8-5290-4a97-af5c-fb53c6a9b89e','idle',1794161388095);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794161388108303557-2122075-3-pane-address-1794160809724966979-2019909-1','pane-address-1794160809724966979-2019909-1','01a1bc51-9c88-71e0-a2cb-6d4a6121e0bb','working',1794161388095);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794161389616607079-2122385-1-pane-address-1794160809313291547-2019619-1','pane-address-1794160809313291547-2019619-1','','unknown',1794161389604);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794161389617530226-2122385-2-pane-address-1794160809516957178-2019766-1','pane-address-1794160809516957178-2019766-1','e603eff8-5290-4a97-af5c-fb53c6a9b89e','idle',1794161389604);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794161389617829640-2122385-3-pane-address-1794160809724966979-2019909-1','pane-address-1794160809724966979-2019909-1','01a1bc51-9c88-71e0-a2cb-6d4a6121e0bb','idle',1794161389604);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794161397767587524-2123808-1-pane-address-1794160809313291547-2019619-1','pane-address-1794160809313291547-2019619-1','','unknown',1794161397755);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794161397768508237-2123808-2-pane-address-1794160809516957178-2019766-1','pane-address-1794160809516957178-2019766-1','e603eff8-5290-4a97-af5c-fb53c6a9b89e','working',1794161397755);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794161397768740634-2123808-3-pane-address-1794160809724966979-2019909-1','pane-address-1794160809724966979-2019909-1','01a1bc51-9c88-71e0-a2cb-6d4a6121e0bb','idle',1794161397755);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794161400784388719-2124249-1-pane-address-1794160809313291547-2019619-1','pane-address-1794160809313291547-2019619-1','','unknown',1794161400772);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794161400785283323-2124249-2-pane-address-1794160809516957178-2019766-1','pane-address-1794160809516957178-2019766-1','e603eff8-5290-4a97-af5c-fb53c6a9b89e','idle',1794161400772);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794161400785530147-2124249-3-pane-address-1794160809724966979-2019909-1','pane-address-1794160809724966979-2019909-1','01a1bc51-9c88-71e0-a2cb-6d4a6121e0bb','idle',1794161400772);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794161406215084786-2125162-1-pane-address-1794160809313291547-2019619-1','pane-address-1794160809313291547-2019619-1','','unknown',1794161406203);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794161406216056605-2125162-2-pane-address-1794160809516957178-2019766-1','pane-address-1794160809516957178-2019766-1','e603eff8-5290-4a97-af5c-fb53c6a9b89e','working',1794161406203);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794161406216348864-2125162-3-pane-address-1794160809724966979-2019909-1','pane-address-1794160809724966979-2019909-1','01a1bc51-9c88-71e0-a2cb-6d4a6121e0bb','idle',1794161406203);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794161406820964226-2125272-1-pane-address-1794160809313291547-2019619-1','pane-address-1794160809313291547-2019619-1','','unknown',1794161406809);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794161406821986419-2125272-2-pane-address-1794160809516957178-2019766-1','pane-address-1794160809516957178-2019766-1','e603eff8-5290-4a97-af5c-fb53c6a9b89e','idle',1794161406809);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794161406822208247-2125272-3-pane-address-1794160809724966979-2019909-1','pane-address-1794160809724966979-2019909-1','01a1bc51-9c88-71e0-a2cb-6d4a6121e0bb','idle',1794161406809);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794161412205468210-2126145-1-pane-address-1794160809313291547-2019619-1','pane-address-1794160809313291547-2019619-1','','unknown',1794161412193);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794161412206483921-2126145-2-pane-address-1794160809516957178-2019766-1','pane-address-1794160809516957178-2019766-1','e603eff8-5290-4a97-af5c-fb53c6a9b89e','idle',1794161412193);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794161412206741346-2126145-3-pane-address-1794160809724966979-2019909-1','pane-address-1794160809724966979-2019909-1','01a1bc51-9c88-71e0-a2cb-6d4a6121e0bb','working',1794161412193);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794161413408407684-2126396-1-pane-address-1794160809313291547-2019619-1','pane-address-1794160809313291547-2019619-1','','unknown',1794161413396);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794161413409419898-2126396-2-pane-address-1794160809516957178-2019766-1','pane-address-1794160809516957178-2019766-1','e603eff8-5290-4a97-af5c-fb53c6a9b89e','idle',1794161413396);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794161413409741373-2126396-3-pane-address-1794160809724966979-2019909-1','pane-address-1794160809724966979-2019909-1','01a1bc51-9c88-71e0-a2cb-6d4a6121e0bb','idle',1794161413396);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794161505297513337-2139787-1-pane-address-1794160809313291547-2019619-1','pane-address-1794160809313291547-2019619-1','','unknown',1794161505285);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794161505298366152-2139787-2-pane-address-1794160809516957178-2019766-1','pane-address-1794160809516957178-2019766-1','e603eff8-5290-4a97-af5c-fb53c6a9b89e','working',1794161505285);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794161505298574895-2139787-3-pane-address-1794160809724966979-2019909-1','pane-address-1794160809724966979-2019909-1','01a1bc51-9c88-71e0-a2cb-6d4a6121e0bb','idle',1794161505285);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794161507508112070-2140085-1-pane-address-1794160809313291547-2019619-1','pane-address-1794160809313291547-2019619-1','','unknown',1794161507495);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794161507509313571-2140085-2-pane-address-1794160809516957178-2019766-1','pane-address-1794160809516957178-2019766-1','e603eff8-5290-4a97-af5c-fb53c6a9b89e','idle',1794161507495);
INSERT INTO agent_session_observations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','observation-1794161507509753790-2140085-3-pane-address-1794160809724966979-2019909-1','pane-address-1794160809724966979-2019909-1','01a1bc51-9c88-71e0-a2cb-6d4a6121e0bb','idle',1794161507495);
CREATE TABLE alias_claims (
    session_socket_id TEXT NOT NULL,
    worktree_id TEXT NOT NULL,
    alias TEXT NOT NULL,
    address_id TEXT NOT NULL,
    claim_state TEXT NOT NULL CHECK (claim_state IN ('active', 'tombstone')),
    replacement_alias TEXT,
    PRIMARY KEY (session_socket_id, worktree_id, alias),
    FOREIGN KEY (session_socket_id, worktree_id, address_id)
        REFERENCES pane_addresses (session_socket_id, worktree_id, address_id)
);
INSERT INTO alias_claims VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','shell','pane-address-1794160809313291547-2019619-1','active',NULL);
INSERT INTO alias_claims VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','claude','pane-address-1794160809516957178-2019766-1','active',NULL);
INSERT INTO alias_claims VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','codex','pane-address-1794160809724966979-2019909-1','active',NULL);
CREATE TABLE alias_history (
    session_socket_id TEXT NOT NULL,
    worktree_id TEXT NOT NULL,
    history_order INTEGER NOT NULL CHECK (history_order > 0),
    alias TEXT NOT NULL,
    address_id TEXT NOT NULL,
    transition_kind TEXT NOT NULL,
    replacement_alias TEXT,
    occurred_at_ms INTEGER NOT NULL,
    PRIMARY KEY (session_socket_id, worktree_id, history_order),
    FOREIGN KEY (session_socket_id, worktree_id, address_id)
        REFERENCES pane_addresses (session_socket_id, worktree_id, address_id)
);
INSERT INTO alias_history VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',1,'shell','pane-address-1794160809313291547-2019619-1','claimed',NULL,1794160809312);
INSERT INTO alias_history VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',2,'claude','pane-address-1794160809516957178-2019766-1','claimed',NULL,1794160809515);
INSERT INTO alias_history VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',3,'codex','pane-address-1794160809724966979-2019909-1','claimed',NULL,1794160809723);
CREATE TABLE attention_items (
    session_socket_id TEXT NOT NULL,
    worktree_id TEXT NOT NULL,
    attention_item_id TEXT NOT NULL,
    source_kind TEXT NOT NULL,
    source_id TEXT NOT NULL,
    summary TEXT NOT NULL,
    opened_at_ms INTEGER NOT NULL,
    resolved_at_ms INTEGER,
    PRIMARY KEY (session_socket_id, worktree_id, attention_item_id),
    FOREIGN KEY (session_socket_id, worktree_id)
        REFERENCES worktrees (session_socket_id, worktree_id)
);
INSERT INTO attention_items VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','terminal-relay:delivery-attempt-1794161387523110130-2120858-1','terminal_relay','terminal-relay:delivery-attempt-1794161387523110130-2120858-1','@codex answered in their terminal and may need your reply.',1794161389300,NULL);
INSERT INTO attention_items VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','terminal-relay:delivery-attempt-1794161397112099446-2120858-1','terminal_relay','terminal-relay:delivery-attempt-1794161397112099446-2120858-1','@claude answered in their terminal and may need your reply.',1794161400555,1794161405087);
INSERT INTO attention_items VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','terminal-relay:delivery-attempt-1794161405634852047-2120858-3','terminal_relay','terminal-relay:delivery-attempt-1794161405634852047-2120858-3','@claude answered in their terminal and may need your reply.',1794161406600,NULL);
INSERT INTO attention_items VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','terminal-relay:delivery-attempt-1794161411672246606-2120858-3','terminal_relay','terminal-relay:delivery-attempt-1794161411672246606-2120858-3','@codex answered in their terminal and may need your reply.',1794161413184,NULL);
INSERT INTO attention_items VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','attention-agent-disposition-delivery-attempt-1794161504622630621-2120858-3','agent_disposition','delivery-attempt-1794161504622630621-2120858-3','The agent received this message and ended its turn without a reply or a no-reply. Its exact session can still answer it from delivery.inbox. Once that agent is retired or its session was replaced, delivery acknowledge clears this warning.',1794161534644,NULL);
CREATE TABLE collaboration_messages (
    session_socket_id TEXT NOT NULL,
    worktree_id TEXT NOT NULL,
    message_id TEXT NOT NULL,
    thread_id TEXT NOT NULL,
    thread_order INTEGER NOT NULL CHECK (thread_order > 0),
    in_reply_to TEXT,
    author_address_id TEXT NOT NULL,
    author_alias_snapshot TEXT NOT NULL,
    author_kind TEXT NOT NULL CHECK (author_kind IN ('human', 'agent_session')),
    author_session_snapshot TEXT,
    request_id TEXT NOT NULL,
    authored_source TEXT NOT NULL,
    body TEXT NOT NULL,
    grammar_version TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL,
    PRIMARY KEY (session_socket_id, worktree_id, message_id),
    UNIQUE (session_socket_id, worktree_id, thread_id, thread_order),
    UNIQUE (session_socket_id, worktree_id, thread_id, message_id),
    FOREIGN KEY (session_socket_id, worktree_id, thread_id)
        REFERENCES collaboration_threads (session_socket_id, worktree_id, thread_id),
    FOREIGN KEY (session_socket_id, worktree_id, thread_id, in_reply_to)
        REFERENCES collaboration_messages (
            session_socket_id, worktree_id, thread_id, message_id
        ),
    FOREIGN KEY (session_socket_id, worktree_id, author_address_id)
        REFERENCES pane_addresses (session_socket_id, worktree_id, address_id)
);
INSERT INTO collaboration_messages VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','message-1794160841432217313-2027716-2','thread-1794160841431515085-2027716-1',1,NULL,'pane-address-1794160809516957178-2019766-1','claude','agent_session','e603eff8-5290-4a97-af5c-fb53c6a9b89e','ping-codex-pong-1','@codex please reply with the single word pong.','please reply with the single word pong.','delegated/v1',1794160841431);
INSERT INTO collaboration_messages VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','message-1794160844382116346-2028392-1','thread-1794160841431515085-2027716-1',2,'message-1794160841432217313-2027716-2','pane-address-1794160809724966979-2019909-1','codex','agent_session','01a1bc51-9c88-71e0-a2cb-6d4a6121e0bb','reply-delivery-attempt-1794160841999085188-2027772-1','@claude pong','pong','mention/v1',1794160844381);
INSERT INTO collaboration_messages VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','message-1794160860267489211-2031385-2','thread-1794160860266878505-2031385-1',1,NULL,'pane-address-1794160809724966979-2019909-1','codex','agent_session','01a1bc51-9c88-71e0-a2cb-6d4a6121e0bb','ask-claude-ping-1794160842','@claude Please reply with the single word ping.','Please reply with the single word ping.','delegated/v1',1794160860266);
INSERT INTO collaboration_messages VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','message-1794160862044355696-2031734-1','thread-1794160860266878505-2031385-1',2,'message-1794160860267489211-2031385-2','pane-address-1794160809516957178-2019766-1','claude','agent_session','e603eff8-5290-4a97-af5c-fb53c6a9b89e','reply-delivery-attempt-1794160860819003657-2031402-1','@codex ping','ping','mention/v1',1794160862044);
INSERT INTO collaboration_messages VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','message-1794160869911650349-2033367-2','thread-1794160869911281928-2033367-1',1,NULL,'pane-address-1794160809516957178-2019766-1','claude','agent_session','e603eff8-5290-4a97-af5c-fb53c6a9b89e','shell-echo-hello-1','@shell echo hello','echo hello','delegated/v1',1794160869911);
INSERT INTO collaboration_messages VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','terminal-result:delivery-attempt-1794160870485822029-2033385-1','thread-1794160869911281928-2033367-1',2,'message-1794160869911650349-2033367-2','pane-address-1794160809313291547-2019619-1','shell','agent_session',NULL,'terminal-result:delivery-attempt-1794160870485822029-2033385-1',unistr('@shell command finished with exit code 0.\u000astdout:\u000ahello\u000a\u000astderr:'),unistr('@shell command finished with exit code 0.\u000astdout:\u000ahello\u000a\u000astderr:'),'terminal-result/v1',1794160870578);
INSERT INTO collaboration_messages VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','message-1794161385288079634-2120858-2','thread-1794161385287441604-2120858-1',1,NULL,'operator:local','operator','human',NULL,'9d331038-3796-484d-b9b2-d1614f25546b','Quick check: could you, @codex, reply with the single word RECAPSEED-BRAVO?','Quick check: could you, @codex, reply with the single word RECAPSEED-BRAVO?','channel/v1',1794161385287);
INSERT INTO collaboration_messages VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','terminal-relay:delivery-attempt-1794161387523110130-2120858-1','thread-1794161385287441604-2120858-1',2,'message-1794161385288079634-2120858-2','pane-address-1794160809724966979-2019909-1','codex','agent_session','01a1bc51-9c88-71e0-a2cb-6d4a6121e0bb','terminal-relay:delivery-attempt-1794161387523110130-2120858-1','RECAPSEED-BRAVO','RECAPSEED-BRAVO','terminal-relay/v1',1794161389300);
INSERT INTO collaboration_messages VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','message-1794161394749014743-2120858-2','thread-1794161394748266676-2120858-1',1,NULL,'operator:local','operator','human',NULL,'5388e595-94fa-4194-94c6-a958adec52ce','Quick question for you, @claude: what single word did codex answer with in the message just above in this channel? Answer with only that word.','Quick question for you, @claude: what single word did codex answer with in the message just above in this channel? Answer with only that word.','channel/v1',1794161394748);
INSERT INTO collaboration_messages VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','terminal-relay:delivery-attempt-1794161397112099446-2120858-1','thread-1794161394748266676-2120858-1',2,'message-1794161394749014743-2120858-2','pane-address-1794160809516957178-2019766-1','claude','agent_session','e603eff8-5290-4a97-af5c-fb53c6a9b89e','terminal-relay:delivery-attempt-1794161397112099446-2120858-1','I can''t see a reply from codex: the channel recap I received only shows your request asking codex to reply with RECAPSEED-BRAVO. That''s the word you asked for, but I can''t confirm codex actually answered with it.','I can''t see a reply from codex: the channel recap I received only shows your request asking codex to reply with RECAPSEED-BRAVO. That''s the word you asked for, but I can''t confirm codex actually answered with it.','terminal-relay/v1',1794161400555);
INSERT INTO collaboration_messages VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','message-1794161405087772424-2120858-1','thread-1794161394748266676-2120858-1',3,'message-1794161394749014743-2120858-2','operator:local','operator','human',NULL,'0ff947ef-c2bc-4092-aa51-0f68e28c6e69','@claude Thread check: reply with the single word THREADCLAUDE.','Thread check: reply with the single word THREADCLAUDE.','mention/v1',1794161405087);
INSERT INTO collaboration_messages VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','terminal-relay:delivery-attempt-1794161405634852047-2120858-3','thread-1794161394748266676-2120858-1',4,'message-1794161405087772424-2120858-1','pane-address-1794160809516957178-2019766-1','claude','agent_session','e603eff8-5290-4a97-af5c-fb53c6a9b89e','terminal-relay:delivery-attempt-1794161405634852047-2120858-3','THREADCLAUDE','THREADCLAUDE','terminal-relay/v1',1794161406600);
INSERT INTO collaboration_messages VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','message-1794161411132859081-2120858-1','direct-message:pane-address-1794160809724966979-2019909-1',1,NULL,'operator:local','operator','human',NULL,'a58bbc8e-fd54-4852-903c-88f7a0cc6342','Please reply to this direct message with the single word DMCODEX.','Please reply to this direct message with the single word DMCODEX.','direct-message/v1',1794161411120);
INSERT INTO collaboration_messages VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','terminal-relay:delivery-attempt-1794161411672246606-2120858-3','direct-message:pane-address-1794160809724966979-2019909-1',2,'message-1794161411132859081-2120858-1','pane-address-1794160809724966979-2019909-1','codex','agent_session','01a1bc51-9c88-71e0-a2cb-6d4a6121e0bb','terminal-relay:delivery-attempt-1794161411672246606-2120858-3','DMCODEX','DMCODEX','terminal-relay/v1',1794161413184);
INSERT INTO collaboration_messages VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','message-1794161504079935426-2120858-1','direct-message:pane-address-1794160809516957178-2019766-1',1,NULL,'operator:local','operator','human',NULL,'2d9c8674-4ccf-4a98-a379-b7d587737ea1','Upgrade check: start ''sleep 600'' as a background Bash task (run_in_background), do not wait for it, and end your turn with the single word WAITING.','Upgrade check: start ''sleep 600'' as a background Bash task (run_in_background), do not wait for it, and end your turn with the single word WAITING.','direct-message/v1',1794161504060);
CREATE TABLE collaboration_threads (
    session_socket_id TEXT NOT NULL,
    worktree_id TEXT NOT NULL,
    thread_id TEXT NOT NULL,
    title TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL,
    PRIMARY KEY (session_socket_id, worktree_id, thread_id),
    FOREIGN KEY (session_socket_id, worktree_id)
        REFERENCES worktrees (session_socket_id, worktree_id)
);
INSERT INTO collaboration_threads VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','thread-1794160841431515085-2027716-1','please reply with the single word pong.',1794160841431);
INSERT INTO collaboration_threads VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','thread-1794160860266878505-2031385-1','Please reply with the single word ping.',1794160860266);
INSERT INTO collaboration_threads VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','thread-1794160869911281928-2033367-1','echo hello',1794160869911);
INSERT INTO collaboration_threads VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','thread-1794161385287441604-2120858-1','Quick check: could you, @codex, reply with the single word RECAPSEED-BRAVO?',1794161385287);
INSERT INTO collaboration_threads VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','thread-1794161394748266676-2120858-1','Quick question for you, @claude: what single word did codex answer with in the m',1794161394748);
INSERT INTO collaboration_threads VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','direct-message:pane-address-1794160809724966979-2019909-1','Direct message',1794161411120);
INSERT INTO collaboration_threads VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','direct-message:pane-address-1794160809516957178-2019766-1','Direct message',1794161504060);
CREATE TABLE context_grants (
    session_socket_id TEXT NOT NULL,
    worktree_id TEXT NOT NULL,
    grant_id TEXT NOT NULL,
    thread_id TEXT NOT NULL,
    source_address_id TEXT NOT NULL,
    grantee_address_id TEXT NOT NULL,
    agent_session_id TEXT NOT NULL,
    pane_binding_id TEXT,
    state TEXT NOT NULL CHECK (
        state IN ('pending', 'active', 'revoked', 'expired')
        AND (
            state NOT IN ('pending', 'active')
            OR pane_binding_id IS NOT NULL
        )
    ),
    PRIMARY KEY (session_socket_id, worktree_id, grant_id),
    FOREIGN KEY (session_socket_id, worktree_id, thread_id)
        REFERENCES collaboration_threads (session_socket_id, worktree_id, thread_id),
    FOREIGN KEY (session_socket_id, worktree_id, source_address_id)
        REFERENCES pane_addresses (session_socket_id, worktree_id, address_id),
    FOREIGN KEY (session_socket_id, worktree_id, grantee_address_id)
        REFERENCES pane_addresses (session_socket_id, worktree_id, address_id),
    FOREIGN KEY (
        session_socket_id, worktree_id, pane_binding_id, source_address_id
    ) REFERENCES pane_bindings (
        session_socket_id, worktree_id, binding_id, address_id
    )
);
CREATE TABLE context_read_audit (
    session_socket_id TEXT NOT NULL,
    worktree_id TEXT NOT NULL,
    audit_order INTEGER NOT NULL CHECK (audit_order > 0),
    grant_id TEXT NOT NULL,
    reader_address_id TEXT,
    actor_kind TEXT NOT NULL CHECK (actor_kind IN ('human', 'agent_session')),
    actor_id TEXT NOT NULL,
    evidence_locator TEXT NOT NULL,
    occurred_at_ms INTEGER NOT NULL,
    PRIMARY KEY (session_socket_id, worktree_id, audit_order),
    FOREIGN KEY (session_socket_id, worktree_id, grant_id)
        REFERENCES context_grants (session_socket_id, worktree_id, grant_id),
    FOREIGN KEY (session_socket_id, worktree_id, reader_address_id)
        REFERENCES pane_addresses (session_socket_id, worktree_id, address_id)
);
CREATE TABLE context_references (
    session_socket_id TEXT NOT NULL,
    worktree_id TEXT NOT NULL,
    message_id TEXT NOT NULL,
    reference_order INTEGER NOT NULL CHECK (reference_order > 0),
    address_id TEXT NOT NULL,
    alias_snapshot TEXT NOT NULL,
    binding_snapshot TEXT,
    session_snapshot TEXT,
    PRIMARY KEY (session_socket_id, worktree_id, message_id, reference_order),
    FOREIGN KEY (session_socket_id, worktree_id, message_id)
        REFERENCES collaboration_messages (session_socket_id, worktree_id, message_id),
    FOREIGN KEY (session_socket_id, worktree_id, address_id)
        REFERENCES pane_addresses (session_socket_id, worktree_id, address_id),
    FOREIGN KEY (session_socket_id, worktree_id, binding_snapshot, address_id)
        REFERENCES pane_bindings (session_socket_id, worktree_id, binding_id, address_id)
);
CREATE TABLE context_snapshot_catalog (
    session_socket_id TEXT NOT NULL,
    worktree_id TEXT NOT NULL,
    snapshot_id TEXT NOT NULL,
    snapshot_kind TEXT NOT NULL CHECK (snapshot_kind IN ('working', 'thread')),
    PRIMARY KEY (session_socket_id, worktree_id, snapshot_id),
    FOREIGN KEY (session_socket_id, worktree_id)
        REFERENCES worktrees (session_socket_id, worktree_id)
);
CREATE TABLE delivery_acknowledgements (
    session_socket_id TEXT NOT NULL,
    worktree_id TEXT NOT NULL,
    acknowledgement_id TEXT NOT NULL,
    attempt_id TEXT NOT NULL,
    agent_session_id TEXT NOT NULL,
    acknowledged_at_ms INTEGER NOT NULL,
    PRIMARY KEY (session_socket_id, worktree_id, acknowledgement_id),
    UNIQUE (session_socket_id, worktree_id, attempt_id),
    FOREIGN KEY (session_socket_id, worktree_id, attempt_id)
        REFERENCES delivery_attempts (session_socket_id, worktree_id, attempt_id)
);
INSERT INTO delivery_acknowledgements VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','router-receipt-delivery-attempt-1794160841999085188-2027772-1','delivery-attempt-1794160841999085188-2027772-1','01a1bc51-9c88-71e0-a2cb-6d4a6121e0bb',1794160842340);
INSERT INTO delivery_acknowledgements VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','router-receipt-delivery-attempt-1794160844932367607-2028411-1','delivery-attempt-1794160844932367607-2028411-1','e603eff8-5290-4a97-af5c-fb53c6a9b89e',1794160845261);
INSERT INTO delivery_acknowledgements VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','router-receipt-delivery-attempt-1794160860819003657-2031402-1','delivery-attempt-1794160860819003657-2031402-1','e603eff8-5290-4a97-af5c-fb53c6a9b89e',1794160861157);
INSERT INTO delivery_acknowledgements VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','router-receipt-delivery-attempt-1794160863380059590-2031870-4','delivery-attempt-1794160863380059590-2031870-4','01a1bc51-9c88-71e0-a2cb-6d4a6121e0bb',1794160863718);
INSERT INTO delivery_acknowledgements VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','router-receipt-delivery-attempt-1794160870485822029-2033385-1','delivery-attempt-1794160870485822029-2033385-1','',1794160870549);
INSERT INTO delivery_acknowledgements VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','router-receipt-delivery-attempt-1794160871834709171-2033608-4','delivery-attempt-1794160871834709171-2033608-4','e603eff8-5290-4a97-af5c-fb53c6a9b89e',1794160872168);
INSERT INTO delivery_acknowledgements VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','router-receipt-delivery-attempt-1794161387523110130-2120858-1','delivery-attempt-1794161387523110130-2120858-1','01a1bc51-9c88-71e0-a2cb-6d4a6121e0bb',1794161387861);
INSERT INTO delivery_acknowledgements VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','router-receipt-delivery-attempt-1794161397112099446-2120858-1','delivery-attempt-1794161397112099446-2120858-1','e603eff8-5290-4a97-af5c-fb53c6a9b89e',1794161397444);
INSERT INTO delivery_acknowledgements VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','router-receipt-delivery-attempt-1794161405634852047-2120858-3','delivery-attempt-1794161405634852047-2120858-3','e603eff8-5290-4a97-af5c-fb53c6a9b89e',1794161405973);
INSERT INTO delivery_acknowledgements VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','router-receipt-delivery-attempt-1794161411672246606-2120858-3','delivery-attempt-1794161411672246606-2120858-3','01a1bc51-9c88-71e0-a2cb-6d4a6121e0bb',1794161412010);
INSERT INTO delivery_acknowledgements VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','router-receipt-delivery-attempt-1794161504622630621-2120858-3','delivery-attempt-1794161504622630621-2120858-3','e603eff8-5290-4a97-af5c-fb53c6a9b89e',1794161504959);
CREATE TABLE delivery_attempt_state (
    session_socket_id TEXT NOT NULL,
    worktree_id TEXT NOT NULL,
    attempt_id TEXT NOT NULL,
    address_id TEXT NOT NULL,
    state TEXT NOT NULL CHECK (
        state IN ('dispatching', 'submitted', 'acknowledged', 'settled',
                  'held', 'failed', 'cancelled')
    ),
    active INTEGER NOT NULL CHECK (active IN (0, 1)),
    PRIMARY KEY (session_socket_id, worktree_id, attempt_id),
    FOREIGN KEY (session_socket_id, worktree_id, attempt_id, address_id)
        REFERENCES delivery_attempts (
            session_socket_id, worktree_id, attempt_id, address_id
        )
);
INSERT INTO delivery_attempt_state VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','delivery-attempt-1794160841999085188-2027772-1','pane-address-1794160809724966979-2019909-1','settled',0);
INSERT INTO delivery_attempt_state VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','delivery-attempt-1794160844932367607-2028411-1','pane-address-1794160809516957178-2019766-1','settled',0);
INSERT INTO delivery_attempt_state VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','delivery-attempt-1794160860819003657-2031402-1','pane-address-1794160809516957178-2019766-1','settled',0);
INSERT INTO delivery_attempt_state VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','delivery-attempt-1794160863380059590-2031870-4','pane-address-1794160809724966979-2019909-1','settled',0);
INSERT INTO delivery_attempt_state VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','delivery-attempt-1794160870485822029-2033385-1','pane-address-1794160809313291547-2019619-1','settled',0);
INSERT INTO delivery_attempt_state VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','delivery-attempt-1794160871834709171-2033608-4','pane-address-1794160809516957178-2019766-1','settled',0);
INSERT INTO delivery_attempt_state VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','delivery-attempt-1794161387523110130-2120858-1','pane-address-1794160809724966979-2019909-1','settled',0);
INSERT INTO delivery_attempt_state VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','delivery-attempt-1794161397112099446-2120858-1','pane-address-1794160809516957178-2019766-1','settled',0);
INSERT INTO delivery_attempt_state VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','delivery-attempt-1794161405634852047-2120858-3','pane-address-1794160809516957178-2019766-1','settled',0);
INSERT INTO delivery_attempt_state VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','delivery-attempt-1794161411672246606-2120858-3','pane-address-1794160809724966979-2019909-1','settled',0);
INSERT INTO delivery_attempt_state VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','delivery-attempt-1794161504622630621-2120858-3','pane-address-1794160809516957178-2019766-1','settled',0);
CREATE TABLE delivery_attempts (
    session_socket_id TEXT NOT NULL,
    worktree_id TEXT NOT NULL,
    attempt_id TEXT NOT NULL,
    delivery_id TEXT NOT NULL,
    address_id TEXT NOT NULL,
    binding_id TEXT NOT NULL,
    agent_session_id TEXT NOT NULL,
    retry_of_attempt_id TEXT,
    created_at_ms INTEGER NOT NULL,
    PRIMARY KEY (session_socket_id, worktree_id, attempt_id),
    UNIQUE (session_socket_id, worktree_id, delivery_id, attempt_id),
    UNIQUE (session_socket_id, worktree_id, attempt_id, address_id),
    UNIQUE (
        session_socket_id, worktree_id, attempt_id, delivery_id, address_id
    ),
    FOREIGN KEY (session_socket_id, worktree_id, delivery_id, address_id)
        REFERENCES mailbox_deliveries (
            session_socket_id, worktree_id, delivery_id, address_id
        ),
    FOREIGN KEY (session_socket_id, worktree_id, address_id)
        REFERENCES pane_addresses (session_socket_id, worktree_id, address_id),
    FOREIGN KEY (session_socket_id, worktree_id, binding_id, address_id)
        REFERENCES pane_bindings (
            session_socket_id, worktree_id, binding_id, address_id
        ),
    FOREIGN KEY (
        session_socket_id, worktree_id, retry_of_attempt_id, delivery_id, address_id
    ) REFERENCES delivery_attempts (
        session_socket_id, worktree_id, attempt_id, delivery_id, address_id
    )
);
INSERT INTO delivery_attempts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','delivery-attempt-1794160841999085188-2027772-1','delivery-1794160841432220940-2027716-3','pane-address-1794160809724966979-2019909-1','pane-binding-1794160809725395583-2019909-3','01a1bc51-9c88-71e0-a2cb-6d4a6121e0bb',NULL,1794160841999);
INSERT INTO delivery_attempts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','delivery-attempt-1794160844932367607-2028411-1','delivery-1794160844382119282-2028392-2','pane-address-1794160809516957178-2019766-1','pane-binding-1794160809517439012-2019766-3','e603eff8-5290-4a97-af5c-fb53c6a9b89e',NULL,1794160844932);
INSERT INTO delivery_attempts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','delivery-attempt-1794160860819003657-2031402-1','delivery-1794160860267491385-2031385-3','pane-address-1794160809516957178-2019766-1','pane-binding-1794160809517439012-2019766-3','e603eff8-5290-4a97-af5c-fb53c6a9b89e',NULL,1794160860819);
INSERT INTO delivery_attempts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','delivery-attempt-1794160863380059590-2031870-4','delivery-1794160862044358842-2031734-2','pane-address-1794160809724966979-2019909-1','pane-binding-1794160809725395583-2019909-3','01a1bc51-9c88-71e0-a2cb-6d4a6121e0bb',NULL,1794160863380);
INSERT INTO delivery_attempts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','delivery-attempt-1794160870485822029-2033385-1','delivery-1794160869911652433-2033367-3','pane-address-1794160809313291547-2019619-1','pane-binding-1794160809313763853-2019619-3','',NULL,1794160870485);
INSERT INTO delivery_attempts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','delivery-attempt-1794160871834709171-2033608-4','terminal-result-delivery:delivery-attempt-1794160870485822029-2033385-1','pane-address-1794160809516957178-2019766-1','pane-binding-1794160809517439012-2019766-3','e603eff8-5290-4a97-af5c-fb53c6a9b89e',NULL,1794160871834);
INSERT INTO delivery_attempts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','delivery-attempt-1794161387523110130-2120858-1','delivery-1794161386977424371-2120858-1','pane-address-1794160809724966979-2019909-1','pane-binding-1794160809725395583-2019909-3','01a1bc51-9c88-71e0-a2cb-6d4a6121e0bb',NULL,1794161387523);
INSERT INTO delivery_attempts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','delivery-attempt-1794161397112099446-2120858-1','delivery-1794161396547352965-2120858-1','pane-address-1794160809516957178-2019766-1','pane-binding-1794160809517439012-2019766-3','e603eff8-5290-4a97-af5c-fb53c6a9b89e',NULL,1794161397112);
INSERT INTO delivery_attempts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','delivery-attempt-1794161405634852047-2120858-3','delivery-1794161405087775300-2120858-2','pane-address-1794160809516957178-2019766-1','pane-binding-1794160809517439012-2019766-3','e603eff8-5290-4a97-af5c-fb53c6a9b89e',NULL,1794161405634);
INSERT INTO delivery_attempts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','delivery-attempt-1794161411672246606-2120858-3','delivery-1794161411132862427-2120858-2','pane-address-1794160809724966979-2019909-1','pane-binding-1794160809725395583-2019909-3','01a1bc51-9c88-71e0-a2cb-6d4a6121e0bb',NULL,1794161411672);
INSERT INTO delivery_attempts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','delivery-attempt-1794161504622630621-2120858-3','delivery-1794161504079942129-2120858-2','pane-address-1794160809516957178-2019766-1','pane-binding-1794160809517439012-2019766-3','e603eff8-5290-4a97-af5c-fb53c6a9b89e',NULL,1794161504622);
CREATE TABLE delivery_dispositions (
    session_socket_id TEXT NOT NULL,
    worktree_id TEXT NOT NULL,
    disposition_id TEXT NOT NULL,
    attempt_id TEXT NOT NULL,
    disposition_kind TEXT NOT NULL,
    occurred_at_ms INTEGER NOT NULL,
    PRIMARY KEY (session_socket_id, worktree_id, disposition_id),
    UNIQUE (session_socket_id, worktree_id, attempt_id),
    FOREIGN KEY (session_socket_id, worktree_id, attempt_id)
        REFERENCES delivery_attempts (session_socket_id, worktree_id, attempt_id)
);
INSERT INTO delivery_dispositions VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','message-1794160844382116346-2028392-1','delivery-attempt-1794160841999085188-2027772-1','reply',1794160844381);
INSERT INTO delivery_dispositions VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','final-reply-no-reply:delivery-attempt-1794160844932367607-2028411-1','delivery-attempt-1794160844932367607-2028411-1','no_reply',1794160846017);
INSERT INTO delivery_dispositions VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','message-1794160862044355696-2031734-1','delivery-attempt-1794160860819003657-2031402-1','reply',1794160862044);
INSERT INTO delivery_dispositions VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','final-reply-no-reply:delivery-attempt-1794160863380059590-2031870-4','delivery-attempt-1794160863380059590-2031870-4','no_reply',1794160865635);
INSERT INTO delivery_dispositions VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','terminal-result:delivery-attempt-1794160870485822029-2033385-1','delivery-attempt-1794160870485822029-2033385-1','reply',1794160870578);
INSERT INTO delivery_dispositions VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','final-reply-no-reply:delivery-attempt-1794160871834709171-2033608-4','delivery-attempt-1794160871834709171-2033608-4','no_reply',1794160873184);
INSERT INTO delivery_dispositions VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','terminal-relay-no-reply:delivery-attempt-1794161387523110130-2120858-1','delivery-attempt-1794161387523110130-2120858-1','no_reply',1794161389300);
INSERT INTO delivery_dispositions VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','terminal-relay-no-reply:delivery-attempt-1794161397112099446-2120858-1','delivery-attempt-1794161397112099446-2120858-1','no_reply',1794161400555);
INSERT INTO delivery_dispositions VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','terminal-relay-no-reply:delivery-attempt-1794161405634852047-2120858-3','delivery-attempt-1794161405634852047-2120858-3','no_reply',1794161406600);
INSERT INTO delivery_dispositions VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','terminal-relay-no-reply:delivery-attempt-1794161411672246606-2120858-3','delivery-attempt-1794161411672246606-2120858-3','no_reply',1794161413184);
CREATE TABLE evidence_index (
    session_socket_id TEXT NOT NULL,
    worktree_id TEXT NOT NULL,
    snapshot_id TEXT NOT NULL,
    evidence_order INTEGER NOT NULL CHECK (evidence_order > 0),
    evidence_kind TEXT NOT NULL,
    locator TEXT NOT NULL,
    PRIMARY KEY (session_socket_id, worktree_id, snapshot_id, evidence_order),
    FOREIGN KEY (session_socket_id, worktree_id, snapshot_id)
        REFERENCES context_snapshot_catalog (session_socket_id, worktree_id, snapshot_id)
);
CREATE TABLE idempotency_records (
    session_socket_id TEXT NOT NULL,
    worktree_id TEXT NOT NULL,
    author_address_id TEXT NOT NULL,
    request_id TEXT NOT NULL,
    request_fingerprint TEXT NOT NULL,
    message_id TEXT NOT NULL,
    PRIMARY KEY (session_socket_id, worktree_id, author_address_id, request_id),
    UNIQUE (session_socket_id, worktree_id, message_id),
    FOREIGN KEY (session_socket_id, worktree_id, author_address_id)
        REFERENCES pane_addresses (session_socket_id, worktree_id, address_id),
    FOREIGN KEY (session_socket_id, worktree_id, message_id)
        REFERENCES collaboration_messages (session_socket_id, worktree_id, message_id)
);
INSERT INTO idempotency_records VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','pane-address-1794160809516957178-2019766-1','ping-codex-pong-1','{"author_address_id":"pane-address-1794160809516957178-2019766-1","author_kind":"agent_session","authored_source":"@codex please reply with the single word pong.","body":"please reply with the single word pong.","context_references":[],"grammar_version":"delegated/v1","in_reply_to":null,"recipients":[{"address_id":"pane-address-1794160809724966979-2019909-1"}]}','message-1794160841432217313-2027716-2');
INSERT INTO idempotency_records VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','pane-address-1794160809724966979-2019909-1','reply-delivery-attempt-1794160841999085188-2027772-1','{"author_address_id":"pane-address-1794160809724966979-2019909-1","author_kind":"agent_session","authored_source":"@claude pong","body":"pong","context_references":[],"grammar_version":"mention/v1","in_reply_to":"message-1794160841432217313-2027716-2","recipients":[{"address_id":"pane-address-1794160809516957178-2019766-1"}]}','message-1794160844382116346-2028392-1');
INSERT INTO idempotency_records VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','pane-address-1794160809724966979-2019909-1','ask-claude-ping-1794160842','{"author_address_id":"pane-address-1794160809724966979-2019909-1","author_kind":"agent_session","authored_source":"@claude Please reply with the single word ping.","body":"Please reply with the single word ping.","context_references":[],"grammar_version":"delegated/v1","in_reply_to":null,"recipients":[{"address_id":"pane-address-1794160809516957178-2019766-1"}]}','message-1794160860267489211-2031385-2');
INSERT INTO idempotency_records VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','pane-address-1794160809516957178-2019766-1','reply-delivery-attempt-1794160860819003657-2031402-1','{"author_address_id":"pane-address-1794160809516957178-2019766-1","author_kind":"agent_session","authored_source":"@codex ping","body":"ping","context_references":[],"grammar_version":"mention/v1","in_reply_to":"message-1794160860267489211-2031385-2","recipients":[{"address_id":"pane-address-1794160809724966979-2019909-1"}]}','message-1794160862044355696-2031734-1');
INSERT INTO idempotency_records VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','pane-address-1794160809516957178-2019766-1','shell-echo-hello-1','{"author_address_id":"pane-address-1794160809516957178-2019766-1","author_kind":"agent_session","authored_source":"@shell echo hello","body":"echo hello","context_references":[],"grammar_version":"delegated/v1","in_reply_to":null,"recipients":[{"address_id":"pane-address-1794160809313291547-2019619-1"}]}','message-1794160869911650349-2033367-2');
INSERT INTO idempotency_records VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','pane-address-1794160809313291547-2019619-1','terminal-result:delivery-attempt-1794160870485822029-2033385-1','{"author_address_id":"pane-address-1794160809313291547-2019619-1","author_kind":"agent_session","authored_source":"@shell command finished with exit code 0.\nstdout:\nhello\n\nstderr:","body":"@shell command finished with exit code 0.\nstdout:\nhello\n\nstderr:","context_references":[],"grammar_version":"terminal-result/v1","in_reply_to":"message-1794160869911650349-2033367-2","recipients":[{"address_id":"pane-address-1794160809516957178-2019766-1"}]}','terminal-result:delivery-attempt-1794160870485822029-2033385-1');
INSERT INTO idempotency_records VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','operator:local','9d331038-3796-484d-b9b2-d1614f25546b','{"author_address_id":"operator:local","author_kind":"human","authored_source":"Quick check: could you, @codex, reply with the single word RECAPSEED-BRAVO?","body":"Quick check: could you, @codex, reply with the single word RECAPSEED-BRAVO?","context_references":[],"grammar_version":"channel/v1","in_reply_to":null,"recipients":[]}','message-1794161385288079634-2120858-2');
INSERT INTO idempotency_records VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','operator:local','5388e595-94fa-4194-94c6-a958adec52ce','{"author_address_id":"operator:local","author_kind":"human","authored_source":"Quick question for you, @claude: what single word did codex answer with in the message just above in this channel? Answer with only that word.","body":"Quick question for you, @claude: what single word did codex answer with in the message just above in this channel? Answer with only that word.","context_references":[],"grammar_version":"channel/v1","in_reply_to":null,"recipients":[]}','message-1794161394749014743-2120858-2');
INSERT INTO idempotency_records VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','operator:local','0ff947ef-c2bc-4092-aa51-0f68e28c6e69','{"author_address_id":"operator:local","author_kind":"human","authored_source":"@claude Thread check: reply with the single word THREADCLAUDE.","body":"Thread check: reply with the single word THREADCLAUDE.","context_references":[],"grammar_version":"mention/v1","in_reply_to":"message-1794161394749014743-2120858-2","recipients":[{"address_id":"pane-address-1794160809516957178-2019766-1"}]}','message-1794161405087772424-2120858-1');
INSERT INTO idempotency_records VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','operator:local','a58bbc8e-fd54-4852-903c-88f7a0cc6342','{"author_address_id":"operator:local","author_kind":"human","authored_source":"Please reply to this direct message with the single word DMCODEX.","body":"Please reply to this direct message with the single word DMCODEX.","context_references":[],"grammar_version":"direct-message/v1","in_reply_to":null,"recipients":[{"address_id":"pane-address-1794160809724966979-2019909-1"}]}','message-1794161411132859081-2120858-1');
INSERT INTO idempotency_records VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','operator:local','2d9c8674-4ccf-4a98-a379-b7d587737ea1','{"author_address_id":"operator:local","author_kind":"human","authored_source":"Upgrade check: start ''sleep 600'' as a background Bash task (run_in_background), do not wait for it, and end your turn with the single word WAITING.","body":"Upgrade check: start ''sleep 600'' as a background Bash task (run_in_background), do not wait for it, and end your turn with the single word WAITING.","context_references":[],"grammar_version":"direct-message/v1","in_reply_to":null,"recipients":[{"address_id":"pane-address-1794160809516957178-2019766-1"}]}','message-1794161504079935426-2120858-1');
CREATE TABLE integration_registrations (
    session_socket_id TEXT NOT NULL,
    worktree_id TEXT NOT NULL,
    registration_id TEXT NOT NULL,
    address_id TEXT NOT NULL,
    agent_session_id TEXT NOT NULL,
    runtime TEXT NOT NULL,
    protocol_version TEXT NOT NULL,
    capability_profile TEXT NOT NULL,
    active INTEGER NOT NULL CHECK (active IN (0, 1)),
    PRIMARY KEY (session_socket_id, worktree_id, registration_id),
    FOREIGN KEY (session_socket_id, worktree_id, address_id)
        REFERENCES pane_addresses (session_socket_id, worktree_id, address_id)
);
INSERT INTO integration_registrations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','integration-registration-1794160810922419295-2020304-4','pane-address-1794160809516957178-2019766-1','e603eff8-5290-4a97-af5c-fb53c6a9b89e','claude-code','collab/v1','["describe","register","ack","send","reply","complete.no-reply","delivery.inbox","snapshot.publish","context.inspect","context.read","handoff.status","handoff.results","handoff.preview","group.list","handoff.begin","handoff.resolve","handoff.adopt","reminder.set","reminder.list","reminder.cancel"]',1);
INSERT INTO integration_registrations VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','integration-registration-1794160830852465297-2024058-4','pane-address-1794160809724966979-2019909-1','01a1bc51-9c88-71e0-a2cb-6d4a6121e0bb','codex','collab/v1','["describe","register","ack","send","reply","complete.no-reply","delivery.inbox","snapshot.publish","context.inspect","context.read","handoff.status","handoff.results","handoff.preview","group.list","handoff.begin","handoff.resolve","handoff.adopt","reminder.set","reminder.list","reminder.cancel"]',1);
CREATE TABLE leases (
    session_socket_id TEXT NOT NULL,
    worktree_id TEXT NOT NULL,
    address_id TEXT NOT NULL,
    attempt_id TEXT NOT NULL,
    owner TEXT NOT NULL,
    acquired_at_ms INTEGER NOT NULL,
    expires_at_ms INTEGER NOT NULL CHECK (expires_at_ms > acquired_at_ms),
    PRIMARY KEY (session_socket_id, worktree_id, address_id),
    UNIQUE (session_socket_id, worktree_id, attempt_id),
    FOREIGN KEY (session_socket_id, worktree_id, attempt_id, address_id)
        REFERENCES delivery_attempts (
            session_socket_id, worktree_id, attempt_id, address_id
        )
);
CREATE TABLE mailbox_deliveries (
    session_socket_id TEXT NOT NULL,
    worktree_id TEXT NOT NULL,
    delivery_id TEXT NOT NULL,
    address_id TEXT NOT NULL,
    mailbox_order INTEGER NOT NULL CHECK (mailbox_order > 0),
    message_id TEXT NOT NULL,
    state TEXT NOT NULL CHECK (
        state IN ('queued', 'dispatching', 'submitted', 'acknowledged',
                  'settled', 'held', 'failed', 'cancelled')
    ),
    created_at_ms INTEGER NOT NULL,
    PRIMARY KEY (session_socket_id, worktree_id, delivery_id),
    UNIQUE (session_socket_id, worktree_id, address_id, mailbox_order),
    UNIQUE (session_socket_id, worktree_id, delivery_id, address_id),
    FOREIGN KEY (session_socket_id, worktree_id, address_id)
        REFERENCES pane_addresses (session_socket_id, worktree_id, address_id),
    FOREIGN KEY (session_socket_id, worktree_id, message_id, address_id)
        REFERENCES message_recipients (
            session_socket_id, worktree_id, message_id, address_id
        )
);
INSERT INTO mailbox_deliveries VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','delivery-1794160841432220940-2027716-3','pane-address-1794160809724966979-2019909-1',1,'message-1794160841432217313-2027716-2','settled',1794160841431);
INSERT INTO mailbox_deliveries VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','delivery-1794160844382119282-2028392-2','pane-address-1794160809516957178-2019766-1',1,'message-1794160844382116346-2028392-1','settled',1794160844381);
INSERT INTO mailbox_deliveries VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','delivery-1794160860267491385-2031385-3','pane-address-1794160809516957178-2019766-1',2,'message-1794160860267489211-2031385-2','settled',1794160860266);
INSERT INTO mailbox_deliveries VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','delivery-1794160862044358842-2031734-2','pane-address-1794160809724966979-2019909-1',2,'message-1794160862044355696-2031734-1','settled',1794160862044);
INSERT INTO mailbox_deliveries VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','delivery-1794160869911652433-2033367-3','pane-address-1794160809313291547-2019619-1',1,'message-1794160869911650349-2033367-2','settled',1794160869911);
INSERT INTO mailbox_deliveries VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','terminal-result-delivery:delivery-attempt-1794160870485822029-2033385-1','pane-address-1794160809516957178-2019766-1',3,'terminal-result:delivery-attempt-1794160870485822029-2033385-1','settled',1794160870578);
INSERT INTO mailbox_deliveries VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','delivery-1794161386977424371-2120858-1','pane-address-1794160809724966979-2019909-1',3,'message-1794161385288079634-2120858-2','settled',1794161386977);
INSERT INTO mailbox_deliveries VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','delivery-1794161396547352965-2120858-1','pane-address-1794160809516957178-2019766-1',4,'message-1794161394749014743-2120858-2','settled',1794161396546);
INSERT INTO mailbox_deliveries VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','delivery-1794161405087775300-2120858-2','pane-address-1794160809516957178-2019766-1',5,'message-1794161405087772424-2120858-1','settled',1794161405087);
INSERT INTO mailbox_deliveries VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','delivery-1794161411132862427-2120858-2','pane-address-1794160809724966979-2019909-1',4,'message-1794161411132859081-2120858-1','settled',1794161411120);
INSERT INTO mailbox_deliveries VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','delivery-1794161504079942129-2120858-2','pane-address-1794160809516957178-2019766-1',6,'message-1794161504079935426-2120858-1','settled',1794161504060);
CREATE TABLE managed_tab_labels (
    session_socket_id TEXT NOT NULL,
    worktree_id TEXT NOT NULL,
    tab_id TEXT NOT NULL,
    address_id TEXT NOT NULL,
    managed_label TEXT NOT NULL,
    previous_label TEXT NOT NULL,
    pending_label TEXT,
    PRIMARY KEY (session_socket_id, worktree_id, tab_id),
    FOREIGN KEY (session_socket_id, worktree_id, address_id)
        REFERENCES pane_addresses (session_socket_id, worktree_id, address_id)
);
INSERT INTO managed_tab_labels VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','w1:t1','pane-address-1794160809313291547-2019619-1','1','1',NULL);
CREATE TABLE message_context_attachments (
    session_socket_id TEXT NOT NULL,
    worktree_id TEXT NOT NULL,
    message_id TEXT NOT NULL,
    address_id TEXT NOT NULL,
    grant_id TEXT NOT NULL,
    PRIMARY KEY (session_socket_id, worktree_id, message_id, address_id),
    UNIQUE (session_socket_id, worktree_id, grant_id),
    FOREIGN KEY (session_socket_id, worktree_id, message_id)
        REFERENCES collaboration_messages (session_socket_id, worktree_id, message_id),
    FOREIGN KEY (session_socket_id, worktree_id, address_id)
        REFERENCES pane_addresses (session_socket_id, worktree_id, address_id),
    FOREIGN KEY (session_socket_id, worktree_id, grant_id)
        REFERENCES context_grants (session_socket_id, worktree_id, grant_id)
);
CREATE TABLE message_context_requests (
    session_socket_id TEXT NOT NULL,
    worktree_id TEXT NOT NULL,
    message_id TEXT NOT NULL,
    address_id TEXT NOT NULL,
    grant_id TEXT NOT NULL,
    PRIMARY KEY (session_socket_id, worktree_id, message_id, address_id),
    UNIQUE (session_socket_id, worktree_id, grant_id),
    FOREIGN KEY (session_socket_id, worktree_id, message_id)
        REFERENCES collaboration_messages (session_socket_id, worktree_id, message_id),
    FOREIGN KEY (session_socket_id, worktree_id, address_id)
        REFERENCES pane_addresses (session_socket_id, worktree_id, address_id),
    FOREIGN KEY (session_socket_id, worktree_id, grant_id)
        REFERENCES context_grants (session_socket_id, worktree_id, grant_id)
);
CREATE TABLE message_recipients (
    session_socket_id TEXT NOT NULL,
    worktree_id TEXT NOT NULL,
    message_id TEXT NOT NULL,
    recipient_order INTEGER NOT NULL CHECK (recipient_order > 0),
    address_id TEXT NOT NULL,
    alias_snapshot TEXT NOT NULL,
    session_snapshot TEXT,
    PRIMARY KEY (session_socket_id, worktree_id, message_id, recipient_order),
    UNIQUE (session_socket_id, worktree_id, message_id, address_id),
    FOREIGN KEY (session_socket_id, worktree_id, message_id)
        REFERENCES collaboration_messages (session_socket_id, worktree_id, message_id),
    FOREIGN KEY (session_socket_id, worktree_id, address_id)
        REFERENCES pane_addresses (session_socket_id, worktree_id, address_id)
);
INSERT INTO message_recipients VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','message-1794160841432217313-2027716-2',1,'pane-address-1794160809724966979-2019909-1','codex','01a1bc51-9c88-71e0-a2cb-6d4a6121e0bb');
INSERT INTO message_recipients VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','message-1794160844382116346-2028392-1',1,'pane-address-1794160809516957178-2019766-1','claude','e603eff8-5290-4a97-af5c-fb53c6a9b89e');
INSERT INTO message_recipients VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','message-1794160860267489211-2031385-2',1,'pane-address-1794160809516957178-2019766-1','claude','e603eff8-5290-4a97-af5c-fb53c6a9b89e');
INSERT INTO message_recipients VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','message-1794160862044355696-2031734-1',1,'pane-address-1794160809724966979-2019909-1','codex','01a1bc51-9c88-71e0-a2cb-6d4a6121e0bb');
INSERT INTO message_recipients VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','message-1794160869911650349-2033367-2',1,'pane-address-1794160809313291547-2019619-1','shell',NULL);
INSERT INTO message_recipients VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','terminal-result:delivery-attempt-1794160870485822029-2033385-1',1,'pane-address-1794160809516957178-2019766-1','claude','e603eff8-5290-4a97-af5c-fb53c6a9b89e');
INSERT INTO message_recipients VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','message-1794161385288079634-2120858-2',1,'pane-address-1794160809724966979-2019909-1','codex','01a1bc51-9c88-71e0-a2cb-6d4a6121e0bb');
INSERT INTO message_recipients VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','message-1794161394749014743-2120858-2',1,'pane-address-1794160809516957178-2019766-1','claude','e603eff8-5290-4a97-af5c-fb53c6a9b89e');
INSERT INTO message_recipients VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','message-1794161405087772424-2120858-1',1,'pane-address-1794160809516957178-2019766-1','claude','e603eff8-5290-4a97-af5c-fb53c6a9b89e');
INSERT INTO message_recipients VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','message-1794161411132859081-2120858-1',1,'pane-address-1794160809724966979-2019909-1','codex','01a1bc51-9c88-71e0-a2cb-6d4a6121e0bb');
INSERT INTO message_recipients VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','message-1794161504079935426-2120858-1',1,'pane-address-1794160809516957178-2019766-1','claude','e603eff8-5290-4a97-af5c-fb53c6a9b89e');
CREATE TABLE order_counters (
    session_socket_id TEXT NOT NULL,
    worktree_id TEXT NOT NULL,
    stream_kind TEXT NOT NULL,
    stream_id TEXT NOT NULL,
    next_order INTEGER NOT NULL CHECK (next_order > 0),
    PRIMARY KEY (session_socket_id, worktree_id, stream_kind, stream_id),
    FOREIGN KEY (session_socket_id, worktree_id)
        REFERENCES worktrees (session_socket_id, worktree_id)
);
INSERT INTO order_counters VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','worktree_transition','',329);
INSERT INTO order_counters VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','alias_history','',4);
INSERT INTO order_counters VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','thread_message','thread-1794160841431515085-2027716-1',3);
INSERT INTO order_counters VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','mailbox','pane-address-1794160809724966979-2019909-1',5);
INSERT INTO order_counters VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','thread_participant','thread-1794160841431515085-2027716-1',3);
INSERT INTO order_counters VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','mailbox','pane-address-1794160809516957178-2019766-1',7);
INSERT INTO order_counters VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','thread_message','thread-1794160860266878505-2031385-1',3);
INSERT INTO order_counters VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','thread_participant','thread-1794160860266878505-2031385-1',3);
INSERT INTO order_counters VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','thread_message','thread-1794160869911281928-2033367-1',3);
INSERT INTO order_counters VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','mailbox','pane-address-1794160809313291547-2019619-1',2);
INSERT INTO order_counters VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','thread_participant','thread-1794160869911281928-2033367-1',3);
INSERT INTO order_counters VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','thread_message','thread-1794161385287441604-2120858-1',3);
INSERT INTO order_counters VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','thread_participant','thread-1794161385287441604-2120858-1',3);
INSERT INTO order_counters VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','thread_message','thread-1794161394748266676-2120858-1',5);
INSERT INTO order_counters VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','thread_participant','thread-1794161394748266676-2120858-1',3);
INSERT INTO order_counters VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','thread_message','direct-message:pane-address-1794160809724966979-2019909-1',3);
INSERT INTO order_counters VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','thread_participant','direct-message:pane-address-1794160809724966979-2019909-1',3);
INSERT INTO order_counters VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','thread_message','direct-message:pane-address-1794160809516957178-2019766-1',2);
INSERT INTO order_counters VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','thread_participant','direct-message:pane-address-1794160809516957178-2019766-1',3);
CREATE TABLE pane_addresses (
    session_socket_id TEXT NOT NULL,
    worktree_id TEXT NOT NULL,
    address_id TEXT NOT NULL,
    role_id TEXT NOT NULL,
    retired INTEGER NOT NULL CHECK (retired IN (0, 1)),
    collaboration_disabled INTEGER NOT NULL CHECK (collaboration_disabled IN (0, 1)),
    created_at_ms INTEGER NOT NULL, pane_type TEXT NOT NULL DEFAULT 'agent' CHECK(pane_type IN ('agent','terminal')), persona TEXT, remote_commands_terminal_id TEXT,
    PRIMARY KEY (session_socket_id, worktree_id, address_id),
    FOREIGN KEY (session_socket_id, worktree_id, role_id)
        REFERENCES pane_roles (session_socket_id, worktree_id, role_id)
);
INSERT INTO pane_addresses VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','pane-address-1794160809313291547-2019619-1','pane-role-1794160809313294162-2019619-2',0,0,1794160809312,'terminal',NULL,NULL);
INSERT INTO pane_addresses VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','pane-address-1794160809516957178-2019766-1','pane-role-1794160809516960645-2019766-2',0,0,1794160809515,'agent',NULL,NULL);
INSERT INTO pane_addresses VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','pane-address-1794160809724966979-2019909-1','pane-role-1794160809724969554-2019909-2',0,0,1794160809723,'agent',NULL,NULL);
INSERT INTO pane_addresses VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','operator:local','role-operator',0,0,1794161385265,'agent',NULL,NULL);
CREATE TABLE pane_bindings (
    session_socket_id TEXT NOT NULL,
    worktree_id TEXT NOT NULL,
    binding_id TEXT NOT NULL,
    address_id TEXT NOT NULL,
    pane_id TEXT NOT NULL,
    active INTEGER NOT NULL CHECK (active IN (0, 1)),
    tab_id TEXT,
    bound_at_ms INTEGER NOT NULL,
    unbound_at_ms INTEGER,
    PRIMARY KEY (session_socket_id, worktree_id, binding_id),
    UNIQUE (session_socket_id, worktree_id, binding_id, address_id),
    FOREIGN KEY (session_socket_id, worktree_id, address_id)
        REFERENCES pane_addresses (session_socket_id, worktree_id, address_id)
);
INSERT INTO pane_bindings VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','pane-binding-1794160809313763853-2019619-3','pane-address-1794160809313291547-2019619-1','w1:p1',1,'w1:t1',1794160809312,NULL);
INSERT INTO pane_bindings VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','pane-binding-1794160809517439012-2019766-3','pane-address-1794160809516957178-2019766-1','w1:p2',1,'w1:t1',1794160809515,NULL);
INSERT INTO pane_bindings VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','pane-binding-1794160809725395583-2019909-3','pane-address-1794160809724966979-2019909-1','w1:p3',1,'w1:t1',1794160809723,NULL);
CREATE TABLE pane_roles (
    session_socket_id TEXT NOT NULL,
    worktree_id TEXT NOT NULL,
    role_id TEXT NOT NULL,
    name TEXT NOT NULL,
    description TEXT NOT NULL,
    PRIMARY KEY (session_socket_id, worktree_id, role_id),
    FOREIGN KEY (session_socket_id, worktree_id)
        REFERENCES worktrees (session_socket_id, worktree_id)
);
INSERT INTO pane_roles VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','pane-role-1794160809313294162-2019619-2','@shell','Native collaborator addressed as @shell.');
INSERT INTO pane_roles VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','pane-role-1794160809516960645-2019766-2','@claude','Native collaborator addressed as @claude.');
INSERT INTO pane_roles VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','pane-role-1794160809724969554-2019909-2','@codex','Native collaborator addressed as @codex.');
INSERT INTO pane_roles VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','role-operator','operator','Paneless local operator.');
CREATE TABLE reconciliation_epochs (
    session_socket_id TEXT NOT NULL,
    worktree_id TEXT NOT NULL,
    epoch INTEGER NOT NULL CHECK (epoch >= 0),
    completed INTEGER NOT NULL CHECK (completed IN (0, 1)),
    PRIMARY KEY (session_socket_id, worktree_id),
    FOREIGN KEY (session_socket_id, worktree_id)
        REFERENCES worktrees (session_socket_id, worktree_id)
);
INSERT INTO reconciliation_epochs VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',5,1);
CREATE TABLE thread_context_snapshots (
    session_socket_id TEXT NOT NULL,
    worktree_id TEXT NOT NULL,
    snapshot_id TEXT NOT NULL,
    thread_id TEXT NOT NULL,
    participant_address_id TEXT NOT NULL,
    payload_json TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL,
    PRIMARY KEY (session_socket_id, worktree_id, snapshot_id),
    FOREIGN KEY (session_socket_id, worktree_id, snapshot_id)
        REFERENCES context_snapshot_catalog (session_socket_id, worktree_id, snapshot_id),
    FOREIGN KEY (session_socket_id, worktree_id, thread_id)
        REFERENCES collaboration_threads (session_socket_id, worktree_id, thread_id),
    FOREIGN KEY (session_socket_id, worktree_id, participant_address_id)
        REFERENCES pane_addresses (session_socket_id, worktree_id, address_id)
);
CREATE TABLE thread_controls (
    session_socket_id TEXT NOT NULL,
    worktree_id TEXT NOT NULL,
    thread_id TEXT NOT NULL,
    paused INTEGER NOT NULL CHECK (paused IN (0, 1)),
    stopped INTEGER NOT NULL CHECK (stopped IN (0, 1)),
    reply_allowance INTEGER NOT NULL CHECK (reply_allowance >= 0),
    PRIMARY KEY (session_socket_id, worktree_id, thread_id),
    FOREIGN KEY (session_socket_id, worktree_id, thread_id)
        REFERENCES collaboration_threads (session_socket_id, worktree_id, thread_id)
);
INSERT INTO thread_controls VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','thread-1794160841431515085-2027716-1',0,0,9);
INSERT INTO thread_controls VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','thread-1794160860266878505-2031385-1',0,0,9);
INSERT INTO thread_controls VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','thread-1794160869911281928-2033367-1',0,0,10);
INSERT INTO thread_controls VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','thread-1794161385287441604-2120858-1',0,0,10);
INSERT INTO thread_controls VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','thread-1794161394748266676-2120858-1',0,0,10);
INSERT INTO thread_controls VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','direct-message:pane-address-1794160809724966979-2019909-1',0,0,10);
INSERT INTO thread_controls VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','direct-message:pane-address-1794160809516957178-2019766-1',0,0,10);
CREATE TABLE thread_participants (
    session_socket_id TEXT NOT NULL,
    worktree_id TEXT NOT NULL,
    thread_id TEXT NOT NULL,
    participant_order INTEGER NOT NULL CHECK (participant_order > 0),
    address_id TEXT NOT NULL,
    PRIMARY KEY (session_socket_id, worktree_id, thread_id, participant_order),
    UNIQUE (session_socket_id, worktree_id, thread_id, address_id),
    FOREIGN KEY (session_socket_id, worktree_id, thread_id)
        REFERENCES collaboration_threads (session_socket_id, worktree_id, thread_id),
    FOREIGN KEY (session_socket_id, worktree_id, address_id)
        REFERENCES pane_addresses (session_socket_id, worktree_id, address_id)
);
INSERT INTO thread_participants VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','thread-1794160841431515085-2027716-1',1,'pane-address-1794160809516957178-2019766-1');
INSERT INTO thread_participants VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','thread-1794160841431515085-2027716-1',2,'pane-address-1794160809724966979-2019909-1');
INSERT INTO thread_participants VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','thread-1794160860266878505-2031385-1',1,'pane-address-1794160809724966979-2019909-1');
INSERT INTO thread_participants VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','thread-1794160860266878505-2031385-1',2,'pane-address-1794160809516957178-2019766-1');
INSERT INTO thread_participants VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','thread-1794160869911281928-2033367-1',1,'pane-address-1794160809516957178-2019766-1');
INSERT INTO thread_participants VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','thread-1794160869911281928-2033367-1',2,'pane-address-1794160809313291547-2019619-1');
INSERT INTO thread_participants VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','thread-1794161385287441604-2120858-1',1,'operator:local');
INSERT INTO thread_participants VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','thread-1794161385287441604-2120858-1',2,'pane-address-1794160809724966979-2019909-1');
INSERT INTO thread_participants VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','thread-1794161394748266676-2120858-1',1,'operator:local');
INSERT INTO thread_participants VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','thread-1794161394748266676-2120858-1',2,'pane-address-1794160809516957178-2019766-1');
INSERT INTO thread_participants VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','direct-message:pane-address-1794160809724966979-2019909-1',1,'operator:local');
INSERT INTO thread_participants VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','direct-message:pane-address-1794160809724966979-2019909-1',2,'pane-address-1794160809724966979-2019909-1');
INSERT INTO thread_participants VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','direct-message:pane-address-1794160809516957178-2019766-1',1,'operator:local');
INSERT INTO thread_participants VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','direct-message:pane-address-1794160809516957178-2019766-1',2,'pane-address-1794160809516957178-2019766-1');
CREATE TABLE transition_facts (
    session_socket_id TEXT NOT NULL,
    worktree_id TEXT NOT NULL,
    worktree_order INTEGER NOT NULL CHECK (worktree_order > 0),
    entity_kind TEXT NOT NULL,
    entity_id TEXT NOT NULL,
    transition_kind TEXT NOT NULL,
    actor_kind TEXT NOT NULL,
    actor_id TEXT NOT NULL,
    occurred_at_ms INTEGER NOT NULL,
    PRIMARY KEY (session_socket_id, worktree_id, worktree_order),
    FOREIGN KEY (session_socket_id, worktree_id)
        REFERENCES worktrees (session_socket_id, worktree_id)
);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',1,'worktree','w1','activated','human','local-human',1794160808497);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',2,'pane_address','pane-address-1794160809313291547-2019619-1','created','human','local-human',1794160809312);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',3,'pane_address','pane-address-1794160809313291547-2019619-1','bound','human','local-human',1794160809312);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',4,'pane_address','pane-address-1794160809313291547-2019619-1','empty','human','local-human',1794160809312);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',5,'pane_address','pane-address-1794160809313291547-2019619-1','empty','human','local-human',1794160809312);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',6,'pane_address','pane-address-1794160809313291547-2019619-1','empty','plugin','herdr',1794160809433);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',7,'pane_address','pane-address-1794160809516957178-2019766-1','created','human','local-human',1794160809515);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',8,'pane_address','pane-address-1794160809516957178-2019766-1','bound','human','local-human',1794160809515);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',9,'pane_address','pane-address-1794160809516957178-2019766-1','empty','human','local-human',1794160809515);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',10,'pane_address','pane-address-1794160809313291547-2019619-1','empty','human','local-human',1794160809515);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',11,'pane_address','pane-address-1794160809516957178-2019766-1','empty','human','local-human',1794160809515);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',12,'pane_address','pane-address-1794160809313291547-2019619-1','empty','plugin','herdr',1794160809679);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',13,'pane_address','pane-address-1794160809516957178-2019766-1','empty','plugin','herdr',1794160809679);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',14,'pane_address','pane-address-1794160809724966979-2019909-1','created','human','local-human',1794160809723);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',15,'pane_address','pane-address-1794160809724966979-2019909-1','bound','human','local-human',1794160809723);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',16,'pane_address','pane-address-1794160809724966979-2019909-1','empty','human','local-human',1794160809723);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',17,'pane_address','pane-address-1794160809313291547-2019619-1','empty','human','local-human',1794160809723);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',18,'pane_address','pane-address-1794160809516957178-2019766-1','empty','human','local-human',1794160809723);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',19,'pane_address','pane-address-1794160809724966979-2019909-1','empty','human','local-human',1794160809723);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',20,'pane_address','pane-address-1794160809313291547-2019619-1','empty','plugin','herdr',1794160809854);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',21,'pane_address','pane-address-1794160809516957178-2019766-1','empty','plugin','herdr',1794160809854);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',22,'pane_address','pane-address-1794160809724966979-2019909-1','empty','plugin','herdr',1794160809854);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',23,'pane_address','pane-address-1794160809313291547-2019619-1','empty','plugin','herdr',1794160810363);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',24,'pane_address','pane-address-1794160809516957178-2019766-1','empty','plugin','herdr',1794160810363);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',25,'pane_address','pane-address-1794160809724966979-2019909-1','empty','plugin','herdr',1794160810363);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',26,'pane_address','pane-address-1794160809313291547-2019619-1','empty','plugin','herdr',1794160810867);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',27,'pane_address','pane-address-1794160809516957178-2019766-1','occupied','plugin','herdr',1794160810867);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',28,'pane_address','pane-address-1794160809724966979-2019909-1','empty','plugin','herdr',1794160810867);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',29,'integration_registration','integration-registration-1794160810922419295-2020304-4','challenged','plugin','namedrop-adapter',1794160810922);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',30,'integration_registration','integration-registration-1794160810922419295-2020304-4','registered','plugin','namedrop-adapter',1794160810922);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',31,'pane_address','pane-address-1794160809313291547-2019619-1','empty','plugin','herdr',1794160810992);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',32,'pane_address','pane-address-1794160809516957178-2019766-1','occupied','plugin','herdr',1794160810992);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',33,'pane_address','pane-address-1794160809724966979-2019909-1','empty','plugin','herdr',1794160810992);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',34,'pane_address','pane-address-1794160809313291547-2019619-1','empty','plugin','herdr',1794160813678);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',35,'pane_address','pane-address-1794160809516957178-2019766-1','occupied','plugin','herdr',1794160813678);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',36,'pane_address','pane-address-1794160809724966979-2019909-1','empty','plugin','herdr',1794160813678);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',37,'pane_address','pane-address-1794160809313291547-2019619-1','empty','plugin','herdr',1794160813777);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',38,'pane_address','pane-address-1794160809516957178-2019766-1','occupied','plugin','herdr',1794160813777);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',39,'pane_address','pane-address-1794160809724966979-2019909-1','empty','plugin','herdr',1794160813777);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',40,'pane_address','pane-address-1794160809313291547-2019619-1','empty','plugin','herdr',1794160814183);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',41,'pane_address','pane-address-1794160809516957178-2019766-1','occupied','plugin','herdr',1794160814183);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',42,'pane_address','pane-address-1794160809724966979-2019909-1','empty','plugin','herdr',1794160814183);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',43,'pane_address','pane-address-1794160809313291547-2019619-1','empty','plugin','herdr',1794160814784);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',44,'pane_address','pane-address-1794160809516957178-2019766-1','occupied','plugin','herdr',1794160814784);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',45,'pane_address','pane-address-1794160809724966979-2019909-1','empty','plugin','herdr',1794160814784);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',46,'pane_address','pane-address-1794160809313291547-2019619-1','empty','plugin','herdr',1794160814873);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',47,'pane_address','pane-address-1794160809516957178-2019766-1','occupied','plugin','herdr',1794160814873);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',48,'pane_address','pane-address-1794160809724966979-2019909-1','empty','plugin','herdr',1794160814873);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',49,'pane_address','pane-address-1794160809313291547-2019619-1','empty','plugin','herdr',1794160814953);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',50,'pane_address','pane-address-1794160809516957178-2019766-1','occupied','plugin','herdr',1794160814953);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',51,'pane_address','pane-address-1794160809724966979-2019909-1','empty','plugin','herdr',1794160814953);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',52,'pane_address','pane-address-1794160809313291547-2019619-1','empty','plugin','herdr',1794160815086);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',53,'pane_address','pane-address-1794160809516957178-2019766-1','occupied','plugin','herdr',1794160815086);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',54,'pane_address','pane-address-1794160809724966979-2019909-1','empty','plugin','herdr',1794160815086);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',55,'pane_address','pane-address-1794160809313291547-2019619-1','empty','plugin','herdr',1794160815590);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',56,'pane_address','pane-address-1794160809516957178-2019766-1','occupied','plugin','herdr',1794160815590);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',57,'pane_address','pane-address-1794160809724966979-2019909-1','empty','plugin','herdr',1794160815590);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',58,'pane_address','pane-address-1794160809313291547-2019619-1','empty','plugin','herdr',1794160815686);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',59,'pane_address','pane-address-1794160809516957178-2019766-1','occupied','plugin','herdr',1794160815686);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',60,'pane_address','pane-address-1794160809724966979-2019909-1','empty','plugin','herdr',1794160815686);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',61,'pane_address','pane-address-1794160809313291547-2019619-1','empty','plugin','herdr',1794160817528);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',62,'pane_address','pane-address-1794160809516957178-2019766-1','occupied','plugin','herdr',1794160817528);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',63,'pane_address','pane-address-1794160809724966979-2019909-1','empty','plugin','herdr',1794160817528);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',64,'pane_address','pane-address-1794160809313291547-2019619-1','empty','plugin','herdr',1794160818694);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',65,'pane_address','pane-address-1794160809516957178-2019766-1','occupied','plugin','herdr',1794160818694);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',66,'pane_address','pane-address-1794160809724966979-2019909-1','empty','plugin','herdr',1794160818694);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',67,'pane_address','pane-address-1794160809313291547-2019619-1','empty','plugin','herdr',1794160819023);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',68,'pane_address','pane-address-1794160809516957178-2019766-1','occupied','plugin','herdr',1794160819023);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',69,'pane_address','pane-address-1794160809724966979-2019909-1','empty','plugin','herdr',1794160819023);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',70,'pane_address','pane-address-1794160809313291547-2019619-1','empty','plugin','herdr',1794160819133);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',71,'pane_address','pane-address-1794160809516957178-2019766-1','occupied','plugin','herdr',1794160819133);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',72,'pane_address','pane-address-1794160809724966979-2019909-1','empty','plugin','herdr',1794160819133);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',73,'pane_address','pane-address-1794160809313291547-2019619-1','empty','plugin','herdr',1794160819226);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',74,'pane_address','pane-address-1794160809516957178-2019766-1','occupied','plugin','herdr',1794160819226);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',75,'pane_address','pane-address-1794160809724966979-2019909-1','empty','plugin','herdr',1794160819226);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',76,'pane_address','pane-address-1794160809313291547-2019619-1','empty','plugin','herdr',1794160819504);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',77,'pane_address','pane-address-1794160809516957178-2019766-1','occupied','plugin','herdr',1794160819504);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',78,'pane_address','pane-address-1794160809724966979-2019909-1','empty','plugin','herdr',1794160819504);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',79,'pane_address','pane-address-1794160809313291547-2019619-1','empty','plugin','herdr',1794160826139);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',80,'pane_address','pane-address-1794160809516957178-2019766-1','occupied','plugin','herdr',1794160826139);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',81,'pane_address','pane-address-1794160809724966979-2019909-1','empty','plugin','herdr',1794160826139);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',82,'pane_address','pane-address-1794160809313291547-2019619-1','empty','plugin','herdr',1794160826226);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',83,'pane_address','pane-address-1794160809516957178-2019766-1','occupied','plugin','herdr',1794160826226);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',84,'pane_address','pane-address-1794160809724966979-2019909-1','empty','plugin','herdr',1794160826226);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',85,'pane_address','pane-address-1794160809313291547-2019619-1','empty','plugin','herdr',1794160826306);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',86,'pane_address','pane-address-1794160809516957178-2019766-1','occupied','plugin','herdr',1794160826306);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',87,'pane_address','pane-address-1794160809724966979-2019909-1','empty','plugin','herdr',1794160826306);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',88,'pane_address','pane-address-1794160809313291547-2019619-1','empty','plugin','herdr',1794160826433);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',89,'pane_address','pane-address-1794160809516957178-2019766-1','occupied','plugin','herdr',1794160826433);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',90,'pane_address','pane-address-1794160809724966979-2019909-1','empty','plugin','herdr',1794160826433);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',91,'pane_address','pane-address-1794160809313291547-2019619-1','empty','plugin','herdr',1794160826941);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',92,'pane_address','pane-address-1794160809516957178-2019766-1','occupied','plugin','herdr',1794160826941);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',93,'pane_address','pane-address-1794160809724966979-2019909-1','empty','plugin','herdr',1794160826941);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',94,'pane_address','pane-address-1794160809313291547-2019619-1','empty','plugin','herdr',1794160827041);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',95,'pane_address','pane-address-1794160809516957178-2019766-1','occupied','plugin','herdr',1794160827041);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',96,'pane_address','pane-address-1794160809724966979-2019909-1','empty','plugin','herdr',1794160827041);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',97,'pane_address','pane-address-1794160809313291547-2019619-1','empty','plugin','herdr',1794160830399);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',98,'pane_address','pane-address-1794160809516957178-2019766-1','occupied','plugin','herdr',1794160830399);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',99,'pane_address','pane-address-1794160809724966979-2019909-1','occupied','plugin','herdr',1794160830399);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',100,'integration_registration','integration-registration-1794160830852465297-2024058-4','challenged','plugin','namedrop-adapter',1794160830852);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',101,'integration_registration','integration-registration-1794160830852465297-2024058-4','registered','plugin','namedrop-adapter',1794160830852);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',102,'pane_address','pane-address-1794160809313291547-2019619-1','empty','plugin','herdr',1794160830978);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',103,'pane_address','pane-address-1794160809516957178-2019766-1','occupied','plugin','herdr',1794160830978);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',104,'pane_address','pane-address-1794160809724966979-2019909-1','occupied','plugin','herdr',1794160830978);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',105,'pane_address','pane-address-1794160809313291547-2019619-1','empty','plugin','herdr',1794160831644);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',106,'pane_address','pane-address-1794160809516957178-2019766-1','occupied','plugin','herdr',1794160831644);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',107,'pane_address','pane-address-1794160809724966979-2019909-1','occupied','plugin','herdr',1794160831644);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',108,'pane_address','pane-address-1794160809313291547-2019619-1','empty','plugin','herdr',1794160834482);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',109,'pane_address','pane-address-1794160809516957178-2019766-1','occupied','plugin','herdr',1794160834482);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',110,'pane_address','pane-address-1794160809724966979-2019909-1','occupied','plugin','herdr',1794160834482);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',111,'collaboration_thread','thread-1794160841431515085-2027716-1','created','agent_session','e603eff8-5290-4a97-af5c-fb53c6a9b89e',1794160841431);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',112,'mailbox_delivery','delivery-1794160841432220940-2027716-3','queued','agent_session','e603eff8-5290-4a97-af5c-fb53c6a9b89e',1794160841431);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',113,'collaboration_message','message-1794160841432217313-2027716-2','created','agent_session','e603eff8-5290-4a97-af5c-fb53c6a9b89e',1794160841431);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',114,'delivery_attempt','delivery-attempt-1794160841999085188-2027772-1','dispatching','plugin','namedrop-dispatch',1794160841999);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',115,'mailbox_delivery','delivery-1794160841432220940-2027716-3','dispatching','plugin','namedrop-dispatch',1794160841999);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',116,'delivery_attempt','delivery-attempt-1794160841999085188-2027772-1','submitted','plugin','namedrop-dispatch',1794160842340);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',117,'mailbox_delivery','delivery-1794160841432220940-2027716-3','submitted','plugin','namedrop-dispatch',1794160842340);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',118,'delivery_attempt','delivery-attempt-1794160841999085188-2027772-1','acknowledged','plugin','namedrop-dispatch',1794160842340);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',119,'mailbox_delivery','delivery-1794160841432220940-2027716-3','acknowledged','plugin','namedrop-dispatch',1794160842340);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',120,'pane_address','pane-address-1794160809313291547-2019619-1','empty','plugin','herdr',1794160842600);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',121,'pane_address','pane-address-1794160809516957178-2019766-1','occupied','plugin','herdr',1794160842600);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',122,'pane_address','pane-address-1794160809724966979-2019909-1','occupied','plugin','herdr',1794160842600);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',123,'pane_address','pane-address-1794160809313291547-2019619-1','empty','plugin','herdr',1794160842699);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',124,'pane_address','pane-address-1794160809516957178-2019766-1','occupied','plugin','herdr',1794160842699);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',125,'pane_address','pane-address-1794160809724966979-2019909-1','occupied','plugin','herdr',1794160842699);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',126,'mailbox_delivery','delivery-1794160844382119282-2028392-2','queued','agent_session','01a1bc51-9c88-71e0-a2cb-6d4a6121e0bb',1794160844381);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',127,'collaboration_message','message-1794160844382116346-2028392-1','created','agent_session','01a1bc51-9c88-71e0-a2cb-6d4a6121e0bb',1794160844381);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',128,'agent_disposition','message-1794160844382116346-2028392-1','completed_reply','agent_session','01a1bc51-9c88-71e0-a2cb-6d4a6121e0bb',1794160844381);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',129,'delivery_attempt','delivery-attempt-1794160841999085188-2027772-1','settled','agent_session','01a1bc51-9c88-71e0-a2cb-6d4a6121e0bb',1794160844381);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',130,'mailbox_delivery','delivery-1794160841432220940-2027716-3','settled','agent_session','01a1bc51-9c88-71e0-a2cb-6d4a6121e0bb',1794160844381);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',131,'delivery_attempt','delivery-attempt-1794160844932367607-2028411-1','dispatching','plugin','namedrop-dispatch',1794160844932);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',132,'mailbox_delivery','delivery-1794160844382119282-2028392-2','dispatching','plugin','namedrop-dispatch',1794160844932);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',133,'delivery_attempt','delivery-attempt-1794160844932367607-2028411-1','submitted','plugin','namedrop-dispatch',1794160845261);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',134,'mailbox_delivery','delivery-1794160844382119282-2028392-2','submitted','plugin','namedrop-dispatch',1794160845261);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',135,'delivery_attempt','delivery-attempt-1794160844932367607-2028411-1','acknowledged','plugin','namedrop-dispatch',1794160845261);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',136,'mailbox_delivery','delivery-1794160844382119282-2028392-2','acknowledged','plugin','namedrop-dispatch',1794160845261);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',137,'pane_address','pane-address-1794160809313291547-2019619-1','empty','plugin','herdr',1794160845303);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',138,'pane_address','pane-address-1794160809516957178-2019766-1','occupied','plugin','herdr',1794160845303);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',139,'pane_address','pane-address-1794160809724966979-2019909-1','occupied','plugin','herdr',1794160845303);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',140,'pane_address','pane-address-1794160809313291547-2019619-1','empty','plugin','herdr',1794160845396);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',141,'pane_address','pane-address-1794160809516957178-2019766-1','occupied','plugin','herdr',1794160845396);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',142,'pane_address','pane-address-1794160809724966979-2019909-1','occupied','plugin','herdr',1794160845396);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',143,'pane_address','pane-address-1794160809313291547-2019619-1','empty','plugin','herdr',1794160845954);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',144,'pane_address','pane-address-1794160809516957178-2019766-1','occupied','plugin','herdr',1794160845954);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',145,'pane_address','pane-address-1794160809724966979-2019909-1','occupied','plugin','herdr',1794160845954);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',146,'agent_disposition','final-reply-no-reply:delivery-attempt-1794160844932367607-2028411-1','completed_no_reply','agent_session','e603eff8-5290-4a97-af5c-fb53c6a9b89e',1794160846017);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',147,'delivery_attempt','delivery-attempt-1794160844932367607-2028411-1','settled','agent_session','e603eff8-5290-4a97-af5c-fb53c6a9b89e',1794160846017);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',148,'mailbox_delivery','delivery-1794160844382119282-2028392-2','settled','agent_session','e603eff8-5290-4a97-af5c-fb53c6a9b89e',1794160846017);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',149,'pane_address','pane-address-1794160809313291547-2019619-1','empty','plugin','herdr',1794160848024);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',150,'pane_address','pane-address-1794160809516957178-2019766-1','occupied','plugin','herdr',1794160848024);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',151,'pane_address','pane-address-1794160809724966979-2019909-1','occupied','plugin','herdr',1794160848024);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',152,'collaboration_thread','thread-1794160860266878505-2031385-1','created','agent_session','01a1bc51-9c88-71e0-a2cb-6d4a6121e0bb',1794160860266);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',153,'mailbox_delivery','delivery-1794160860267491385-2031385-3','queued','agent_session','01a1bc51-9c88-71e0-a2cb-6d4a6121e0bb',1794160860266);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',154,'collaboration_message','message-1794160860267489211-2031385-2','created','agent_session','01a1bc51-9c88-71e0-a2cb-6d4a6121e0bb',1794160860266);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',155,'delivery_attempt','delivery-attempt-1794160860819003657-2031402-1','dispatching','plugin','namedrop-dispatch',1794160860819);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',156,'mailbox_delivery','delivery-1794160860267491385-2031385-3','dispatching','plugin','namedrop-dispatch',1794160860819);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',157,'delivery_attempt','delivery-attempt-1794160860819003657-2031402-1','submitted','plugin','namedrop-dispatch',1794160861157);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',158,'mailbox_delivery','delivery-1794160860267491385-2031385-3','submitted','plugin','namedrop-dispatch',1794160861157);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',159,'delivery_attempt','delivery-attempt-1794160860819003657-2031402-1','acknowledged','plugin','namedrop-dispatch',1794160861157);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',160,'mailbox_delivery','delivery-1794160860267491385-2031385-3','acknowledged','plugin','namedrop-dispatch',1794160861157);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',161,'pane_address','pane-address-1794160809313291547-2019619-1','empty','plugin','herdr',1794160861306);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',162,'pane_address','pane-address-1794160809516957178-2019766-1','occupied','plugin','herdr',1794160861306);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',163,'pane_address','pane-address-1794160809724966979-2019909-1','occupied','plugin','herdr',1794160861306);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',164,'mailbox_delivery','delivery-1794160862044358842-2031734-2','queued','agent_session','e603eff8-5290-4a97-af5c-fb53c6a9b89e',1794160862044);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',165,'collaboration_message','message-1794160862044355696-2031734-1','created','agent_session','e603eff8-5290-4a97-af5c-fb53c6a9b89e',1794160862044);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',166,'agent_disposition','message-1794160862044355696-2031734-1','completed_reply','agent_session','e603eff8-5290-4a97-af5c-fb53c6a9b89e',1794160862044);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',167,'delivery_attempt','delivery-attempt-1794160860819003657-2031402-1','settled','agent_session','e603eff8-5290-4a97-af5c-fb53c6a9b89e',1794160862044);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',168,'mailbox_delivery','delivery-1794160860267491385-2031385-3','settled','agent_session','e603eff8-5290-4a97-af5c-fb53c6a9b89e',1794160862044);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',169,'pane_address','pane-address-1794160809313291547-2019619-1','empty','plugin','herdr',1794160862219);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',170,'pane_address','pane-address-1794160809516957178-2019766-1','occupied','plugin','herdr',1794160862219);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',171,'pane_address','pane-address-1794160809724966979-2019909-1','occupied','plugin','herdr',1794160862219);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',172,'pane_address','pane-address-1794160809313291547-2019619-1','empty','plugin','herdr',1794160862802);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',173,'pane_address','pane-address-1794160809516957178-2019766-1','occupied','plugin','herdr',1794160862802);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',174,'pane_address','pane-address-1794160809724966979-2019909-1','occupied','plugin','herdr',1794160862802);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',175,'delivery_attempt','delivery-attempt-1794160863380059590-2031870-4','dispatching','plugin','namedrop-dispatch',1794160863380);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',176,'mailbox_delivery','delivery-1794160862044358842-2031734-2','dispatching','plugin','namedrop-dispatch',1794160863380);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',177,'delivery_attempt','delivery-attempt-1794160863380059590-2031870-4','submitted','plugin','namedrop-dispatch',1794160863718);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',178,'mailbox_delivery','delivery-1794160862044358842-2031734-2','submitted','plugin','namedrop-dispatch',1794160863718);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',179,'delivery_attempt','delivery-attempt-1794160863380059590-2031870-4','acknowledged','plugin','namedrop-dispatch',1794160863718);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',180,'mailbox_delivery','delivery-1794160862044358842-2031734-2','acknowledged','plugin','namedrop-dispatch',1794160863718);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',181,'pane_address','pane-address-1794160809313291547-2019619-1','empty','plugin','herdr',1794160864001);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',182,'pane_address','pane-address-1794160809516957178-2019766-1','occupied','plugin','herdr',1794160864001);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',183,'pane_address','pane-address-1794160809724966979-2019909-1','occupied','plugin','herdr',1794160864001);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',184,'agent_disposition','final-reply-no-reply:delivery-attempt-1794160863380059590-2031870-4','completed_no_reply','agent_session','01a1bc51-9c88-71e0-a2cb-6d4a6121e0bb',1794160865635);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',185,'delivery_attempt','delivery-attempt-1794160863380059590-2031870-4','settled','agent_session','01a1bc51-9c88-71e0-a2cb-6d4a6121e0bb',1794160865635);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',186,'mailbox_delivery','delivery-1794160862044358842-2031734-2','settled','agent_session','01a1bc51-9c88-71e0-a2cb-6d4a6121e0bb',1794160865635);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',187,'pane_address','pane-address-1794160809313291547-2019619-1','empty','plugin','herdr',1794160865816);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',188,'pane_address','pane-address-1794160809516957178-2019766-1','occupied','plugin','herdr',1794160865816);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',189,'pane_address','pane-address-1794160809724966979-2019909-1','occupied','plugin','herdr',1794160865816);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',190,'pane_address','pane-address-1794160809313291547-2019619-1','empty','plugin','herdr',1794160867936);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',191,'pane_address','pane-address-1794160809516957178-2019766-1','occupied','plugin','herdr',1794160867936);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',192,'pane_address','pane-address-1794160809724966979-2019909-1','occupied','plugin','herdr',1794160867936);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',193,'collaboration_thread','thread-1794160869911281928-2033367-1','created','agent_session','e603eff8-5290-4a97-af5c-fb53c6a9b89e',1794160869911);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',194,'mailbox_delivery','delivery-1794160869911652433-2033367-3','queued','agent_session','e603eff8-5290-4a97-af5c-fb53c6a9b89e',1794160869911);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',195,'collaboration_message','message-1794160869911650349-2033367-2','created','agent_session','e603eff8-5290-4a97-af5c-fb53c6a9b89e',1794160869911);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',196,'delivery_attempt','delivery-attempt-1794160870485822029-2033385-1','dispatching','plugin','namedrop-dispatch',1794160870485);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',197,'mailbox_delivery','delivery-1794160869911652433-2033367-3','dispatching','plugin','namedrop-dispatch',1794160870485);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',198,'delivery_attempt','delivery-attempt-1794160870485822029-2033385-1','submitted','plugin','namedrop-dispatch',1794160870513);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',199,'mailbox_delivery','delivery-1794160869911652433-2033367-3','submitted','plugin','namedrop-dispatch',1794160870513);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',200,'delivery_attempt','delivery-attempt-1794160870485822029-2033385-1','acknowledged','plugin','namedrop-terminal',1794160870549);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',201,'mailbox_delivery','delivery-1794160869911652433-2033367-3','acknowledged','plugin','namedrop-terminal',1794160870549);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',202,'mailbox_delivery','terminal-result-delivery:delivery-attempt-1794160870485822029-2033385-1','queued','plugin','namedrop-terminal',1794160870578);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',203,'collaboration_message','terminal-result:delivery-attempt-1794160870485822029-2033385-1','created','plugin','namedrop-terminal',1794160870578);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',204,'agent_disposition','terminal-result:delivery-attempt-1794160870485822029-2033385-1','completed_reply','plugin','namedrop-terminal',1794160870578);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',205,'delivery_attempt','delivery-attempt-1794160870485822029-2033385-1','settled','plugin','namedrop-terminal',1794160870578);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',206,'mailbox_delivery','delivery-1794160869911652433-2033367-3','settled','plugin','namedrop-terminal',1794160870578);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',207,'pane_address','pane-address-1794160809313291547-2019619-1','empty','plugin','herdr',1794160871257);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',208,'pane_address','pane-address-1794160809516957178-2019766-1','occupied','plugin','herdr',1794160871257);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',209,'pane_address','pane-address-1794160809724966979-2019909-1','occupied','plugin','herdr',1794160871257);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',210,'delivery_attempt','delivery-attempt-1794160871834709171-2033608-4','dispatching','plugin','namedrop-dispatch',1794160871834);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',211,'mailbox_delivery','terminal-result-delivery:delivery-attempt-1794160870485822029-2033385-1','dispatching','plugin','namedrop-dispatch',1794160871834);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',212,'delivery_attempt','delivery-attempt-1794160871834709171-2033608-4','submitted','plugin','namedrop-dispatch',1794160872168);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',213,'mailbox_delivery','terminal-result-delivery:delivery-attempt-1794160870485822029-2033385-1','submitted','plugin','namedrop-dispatch',1794160872168);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',214,'delivery_attempt','delivery-attempt-1794160871834709171-2033608-4','acknowledged','plugin','namedrop-dispatch',1794160872168);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',215,'mailbox_delivery','terminal-result-delivery:delivery-attempt-1794160870485822029-2033385-1','acknowledged','plugin','namedrop-dispatch',1794160872168);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',216,'pane_address','pane-address-1794160809313291547-2019619-1','empty','plugin','herdr',1794160872472);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',217,'pane_address','pane-address-1794160809516957178-2019766-1','occupied','plugin','herdr',1794160872472);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',218,'pane_address','pane-address-1794160809724966979-2019909-1','occupied','plugin','herdr',1794160872472);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',219,'agent_disposition','final-reply-no-reply:delivery-attempt-1794160871834709171-2033608-4','completed_no_reply','agent_session','e603eff8-5290-4a97-af5c-fb53c6a9b89e',1794160873184);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',220,'delivery_attempt','delivery-attempt-1794160871834709171-2033608-4','settled','agent_session','e603eff8-5290-4a97-af5c-fb53c6a9b89e',1794160873184);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',221,'mailbox_delivery','terminal-result-delivery:delivery-attempt-1794160870485822029-2033385-1','settled','agent_session','e603eff8-5290-4a97-af5c-fb53c6a9b89e',1794160873184);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',222,'pane_address','pane-address-1794160809313291547-2019619-1','empty','plugin','herdr',1794160873377);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',223,'pane_address','pane-address-1794160809516957178-2019766-1','occupied','plugin','herdr',1794160873377);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',224,'pane_address','pane-address-1794160809724966979-2019909-1','occupied','plugin','herdr',1794160873377);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',225,'collaboration_thread','thread-1794161385287441604-2120858-1','created','human','local-human',1794161385287);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',226,'collaboration_message','message-1794161385288079634-2120858-2','created','human','local-human',1794161385287);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',227,'collaboration_message','message-1794161385288079634-2120858-2','queued','human','channel-router/v1',1794161385287);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',228,'collaboration_message','message-1794161385288079634-2120858-2','dispatching','human','channel-router/v1',1794161385302);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',229,'mailbox_delivery','delivery-1794161386977424371-2120858-1','queued','human','channel-router/v1',1794161386977);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',230,'collaboration_message','message-1794161385288079634-2120858-2','settled','human','channel-router/v1',1794161386977);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',231,'delivery_attempt','delivery-attempt-1794161387523110130-2120858-1','dispatching','plugin','namedrop-dispatch',1794161387523);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',232,'mailbox_delivery','delivery-1794161386977424371-2120858-1','dispatching','plugin','namedrop-dispatch',1794161387523);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',233,'delivery_attempt','delivery-attempt-1794161387523110130-2120858-1','submitted','plugin','namedrop-dispatch',1794161387861);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',234,'mailbox_delivery','delivery-1794161386977424371-2120858-1','submitted','plugin','namedrop-dispatch',1794161387861);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',235,'delivery_attempt','delivery-attempt-1794161387523110130-2120858-1','acknowledged','plugin','namedrop-dispatch',1794161387861);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',236,'mailbox_delivery','delivery-1794161386977424371-2120858-1','acknowledged','plugin','namedrop-dispatch',1794161387861);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',237,'pane_address','pane-address-1794160809313291547-2019619-1','empty','plugin','herdr',1794161388095);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',238,'pane_address','pane-address-1794160809516957178-2019766-1','occupied','plugin','herdr',1794161388095);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',239,'pane_address','pane-address-1794160809724966979-2019909-1','occupied','plugin','herdr',1794161388095);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',240,'collaboration_message','terminal-relay:delivery-attempt-1794161387523110130-2120858-1','created','plugin','namedrop',1794161389300);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',241,'agent_disposition','terminal-relay-no-reply:delivery-attempt-1794161387523110130-2120858-1','completed_no_reply','plugin','namedrop',1794161389300);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',242,'delivery_attempt','delivery-attempt-1794161387523110130-2120858-1','settled','plugin','namedrop',1794161389300);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',243,'mailbox_delivery','delivery-1794161386977424371-2120858-1','settled','plugin','namedrop',1794161389300);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',244,'attention_item','terminal-relay:delivery-attempt-1794161387523110130-2120858-1','opened','plugin','namedrop',1794161389300);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',245,'pane_address','pane-address-1794160809313291547-2019619-1','empty','plugin','herdr',1794161389604);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',246,'pane_address','pane-address-1794160809516957178-2019766-1','occupied','plugin','herdr',1794161389604);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',247,'pane_address','pane-address-1794160809724966979-2019909-1','occupied','plugin','herdr',1794161389604);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',248,'collaboration_thread','thread-1794161394748266676-2120858-1','created','human','local-human',1794161394748);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',249,'collaboration_message','message-1794161394749014743-2120858-2','created','human','local-human',1794161394748);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',250,'collaboration_message','message-1794161394749014743-2120858-2','queued','human','channel-router/v1',1794161394748);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',251,'collaboration_message','message-1794161394749014743-2120858-2','dispatching','human','channel-router/v1',1794161394783);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',252,'mailbox_delivery','delivery-1794161396547352965-2120858-1','queued','human','channel-router/v1',1794161396546);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',253,'collaboration_message','message-1794161394749014743-2120858-2','settled','human','channel-router/v1',1794161396546);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',254,'delivery_attempt','delivery-attempt-1794161397112099446-2120858-1','dispatching','plugin','namedrop-dispatch',1794161397112);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',255,'mailbox_delivery','delivery-1794161396547352965-2120858-1','dispatching','plugin','namedrop-dispatch',1794161397112);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',256,'delivery_attempt','delivery-attempt-1794161397112099446-2120858-1','submitted','plugin','namedrop-dispatch',1794161397444);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',257,'mailbox_delivery','delivery-1794161396547352965-2120858-1','submitted','plugin','namedrop-dispatch',1794161397444);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',258,'delivery_attempt','delivery-attempt-1794161397112099446-2120858-1','acknowledged','plugin','namedrop-dispatch',1794161397444);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',259,'mailbox_delivery','delivery-1794161396547352965-2120858-1','acknowledged','plugin','namedrop-dispatch',1794161397444);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',260,'pane_address','pane-address-1794160809313291547-2019619-1','empty','plugin','herdr',1794161397755);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',261,'pane_address','pane-address-1794160809516957178-2019766-1','occupied','plugin','herdr',1794161397755);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',262,'pane_address','pane-address-1794160809724966979-2019909-1','occupied','plugin','herdr',1794161397755);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',263,'collaboration_message','terminal-relay:delivery-attempt-1794161397112099446-2120858-1','created','plugin','namedrop',1794161400555);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',264,'agent_disposition','terminal-relay-no-reply:delivery-attempt-1794161397112099446-2120858-1','completed_no_reply','plugin','namedrop',1794161400555);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',265,'delivery_attempt','delivery-attempt-1794161397112099446-2120858-1','settled','plugin','namedrop',1794161400555);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',266,'mailbox_delivery','delivery-1794161396547352965-2120858-1','settled','plugin','namedrop',1794161400555);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',267,'attention_item','terminal-relay:delivery-attempt-1794161397112099446-2120858-1','opened','plugin','namedrop',1794161400555);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',268,'pane_address','pane-address-1794160809313291547-2019619-1','empty','plugin','herdr',1794161400772);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',269,'pane_address','pane-address-1794160809516957178-2019766-1','occupied','plugin','herdr',1794161400772);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',270,'pane_address','pane-address-1794160809724966979-2019909-1','occupied','plugin','herdr',1794161400772);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',271,'mailbox_delivery','delivery-1794161405087775300-2120858-2','queued','human','local-human',1794161405087);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',272,'attention_item','terminal-relay:delivery-attempt-1794161397112099446-2120858-1','resolved','human','local-human',1794161405087);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',273,'collaboration_message','message-1794161405087772424-2120858-1','created','human','local-human',1794161405087);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',274,'delivery_attempt','delivery-attempt-1794161405634852047-2120858-3','dispatching','plugin','namedrop-dispatch',1794161405634);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',275,'mailbox_delivery','delivery-1794161405087775300-2120858-2','dispatching','plugin','namedrop-dispatch',1794161405634);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',276,'delivery_attempt','delivery-attempt-1794161405634852047-2120858-3','submitted','plugin','namedrop-dispatch',1794161405973);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',277,'mailbox_delivery','delivery-1794161405087775300-2120858-2','submitted','plugin','namedrop-dispatch',1794161405973);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',278,'delivery_attempt','delivery-attempt-1794161405634852047-2120858-3','acknowledged','plugin','namedrop-dispatch',1794161405973);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',279,'mailbox_delivery','delivery-1794161405087775300-2120858-2','acknowledged','plugin','namedrop-dispatch',1794161405973);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',280,'pane_address','pane-address-1794160809313291547-2019619-1','empty','plugin','herdr',1794161406203);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',281,'pane_address','pane-address-1794160809516957178-2019766-1','occupied','plugin','herdr',1794161406203);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',282,'pane_address','pane-address-1794160809724966979-2019909-1','occupied','plugin','herdr',1794161406203);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',283,'collaboration_message','terminal-relay:delivery-attempt-1794161405634852047-2120858-3','created','plugin','namedrop',1794161406600);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',284,'agent_disposition','terminal-relay-no-reply:delivery-attempt-1794161405634852047-2120858-3','completed_no_reply','plugin','namedrop',1794161406600);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',285,'delivery_attempt','delivery-attempt-1794161405634852047-2120858-3','settled','plugin','namedrop',1794161406600);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',286,'mailbox_delivery','delivery-1794161405087775300-2120858-2','settled','plugin','namedrop',1794161406600);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',287,'attention_item','terminal-relay:delivery-attempt-1794161405634852047-2120858-3','opened','plugin','namedrop',1794161406600);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',288,'pane_address','pane-address-1794160809313291547-2019619-1','empty','plugin','herdr',1794161406809);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',289,'pane_address','pane-address-1794160809516957178-2019766-1','occupied','plugin','herdr',1794161406809);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',290,'pane_address','pane-address-1794160809724966979-2019909-1','occupied','plugin','herdr',1794161406809);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',291,'collaboration_thread','direct-message:pane-address-1794160809724966979-2019909-1','created','human','local-human',1794161411120);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',292,'mailbox_delivery','delivery-1794161411132862427-2120858-2','queued','human','local-human',1794161411120);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',293,'collaboration_message','message-1794161411132859081-2120858-1','created','human','local-human',1794161411120);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',294,'delivery_attempt','delivery-attempt-1794161411672246606-2120858-3','dispatching','plugin','namedrop-dispatch',1794161411672);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',295,'mailbox_delivery','delivery-1794161411132862427-2120858-2','dispatching','plugin','namedrop-dispatch',1794161411672);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',296,'delivery_attempt','delivery-attempt-1794161411672246606-2120858-3','submitted','plugin','namedrop-dispatch',1794161412010);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',297,'mailbox_delivery','delivery-1794161411132862427-2120858-2','submitted','plugin','namedrop-dispatch',1794161412010);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',298,'delivery_attempt','delivery-attempt-1794161411672246606-2120858-3','acknowledged','plugin','namedrop-dispatch',1794161412010);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',299,'mailbox_delivery','delivery-1794161411132862427-2120858-2','acknowledged','plugin','namedrop-dispatch',1794161412010);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',300,'pane_address','pane-address-1794160809313291547-2019619-1','empty','plugin','herdr',1794161412193);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',301,'pane_address','pane-address-1794160809516957178-2019766-1','occupied','plugin','herdr',1794161412193);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',302,'pane_address','pane-address-1794160809724966979-2019909-1','occupied','plugin','herdr',1794161412193);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',303,'collaboration_message','terminal-relay:delivery-attempt-1794161411672246606-2120858-3','created','plugin','namedrop',1794161413184);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',304,'agent_disposition','terminal-relay-no-reply:delivery-attempt-1794161411672246606-2120858-3','completed_no_reply','plugin','namedrop',1794161413184);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',305,'delivery_attempt','delivery-attempt-1794161411672246606-2120858-3','settled','plugin','namedrop',1794161413184);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',306,'mailbox_delivery','delivery-1794161411132862427-2120858-2','settled','plugin','namedrop',1794161413184);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',307,'attention_item','terminal-relay:delivery-attempt-1794161411672246606-2120858-3','opened','plugin','namedrop',1794161413184);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',308,'pane_address','pane-address-1794160809313291547-2019619-1','empty','plugin','herdr',1794161413396);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',309,'pane_address','pane-address-1794160809516957178-2019766-1','occupied','plugin','herdr',1794161413396);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',310,'pane_address','pane-address-1794160809724966979-2019909-1','occupied','plugin','herdr',1794161413396);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',311,'collaboration_thread','direct-message:pane-address-1794160809516957178-2019766-1','created','human','local-human',1794161504060);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',312,'mailbox_delivery','delivery-1794161504079942129-2120858-2','queued','human','local-human',1794161504060);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',313,'collaboration_message','message-1794161504079935426-2120858-1','created','human','local-human',1794161504060);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',314,'delivery_attempt','delivery-attempt-1794161504622630621-2120858-3','dispatching','plugin','namedrop-dispatch',1794161504622);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',315,'mailbox_delivery','delivery-1794161504079942129-2120858-2','dispatching','plugin','namedrop-dispatch',1794161504622);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',316,'delivery_attempt','delivery-attempt-1794161504622630621-2120858-3','submitted','plugin','namedrop-dispatch',1794161504959);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',317,'mailbox_delivery','delivery-1794161504079942129-2120858-2','submitted','plugin','namedrop-dispatch',1794161504959);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',318,'delivery_attempt','delivery-attempt-1794161504622630621-2120858-3','acknowledged','plugin','namedrop-dispatch',1794161504959);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',319,'mailbox_delivery','delivery-1794161504079942129-2120858-2','acknowledged','plugin','namedrop-dispatch',1794161504959);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',320,'pane_address','pane-address-1794160809313291547-2019619-1','empty','plugin','herdr',1794161505285);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',321,'pane_address','pane-address-1794160809516957178-2019766-1','occupied','plugin','herdr',1794161505285);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',322,'pane_address','pane-address-1794160809724966979-2019909-1','occupied','plugin','herdr',1794161505285);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',323,'pane_address','pane-address-1794160809313291547-2019619-1','empty','plugin','herdr',1794161507495);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',324,'pane_address','pane-address-1794160809516957178-2019766-1','occupied','plugin','herdr',1794161507495);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',325,'pane_address','pane-address-1794160809724966979-2019909-1','occupied','plugin','herdr',1794161507495);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',326,'delivery_attempt','delivery-attempt-1794161504622630621-2120858-3','settled','plugin','namedrop-dispatch',1794161534644);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',327,'mailbox_delivery','delivery-1794161504079942129-2120858-2','settled','plugin','namedrop-dispatch',1794161534644);
INSERT INTO transition_facts VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',328,'attention_item','attention-agent-disposition-delivery-attempt-1794161504622630621-2120858-3','opened','plugin','namedrop-dispatch',1794161534644);
CREATE TABLE working_context_snapshots (
    session_socket_id TEXT NOT NULL,
    worktree_id TEXT NOT NULL,
    snapshot_id TEXT NOT NULL,
    grant_id TEXT NOT NULL,
    agent_session_id TEXT NOT NULL,
    payload_json TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL,
    PRIMARY KEY (session_socket_id, worktree_id, snapshot_id),
    FOREIGN KEY (session_socket_id, worktree_id, snapshot_id)
        REFERENCES context_snapshot_catalog (session_socket_id, worktree_id, snapshot_id),
    FOREIGN KEY (session_socket_id, worktree_id, grant_id)
        REFERENCES context_grants (session_socket_id, worktree_id, grant_id)
);
CREATE TABLE worktree_settings (
    session_socket_id TEXT NOT NULL,
    worktree_id TEXT NOT NULL,
    settled_delay_ms INTEGER NOT NULL CHECK (settled_delay_ms >= 0),
    PRIMARY KEY (session_socket_id, worktree_id),
    FOREIGN KEY (session_socket_id, worktree_id)
        REFERENCES worktrees (session_socket_id, worktree_id)
);
INSERT INTO worktree_settings VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',500);
CREATE TABLE worktrees (
    session_socket_id TEXT NOT NULL,
    worktree_id TEXT NOT NULL,
    activated INTEGER NOT NULL CHECK (activated IN (0, 1)),
    paused INTEGER NOT NULL CHECK (paused IN (0, 1)),
    deactivated INTEGER NOT NULL CHECK (deactivated IN (0, 1)),
    created_at_ms INTEGER NOT NULL CHECK (created_at_ms >= 0),
    PRIMARY KEY (session_socket_id, worktree_id)
);
INSERT INTO worktrees VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1',1,0,0,1794160808497);
CREATE TABLE mention_groups (
    session_socket_id TEXT NOT NULL,
    worktree_id TEXT NOT NULL,
    alias TEXT NOT NULL,
    PRIMARY KEY (session_socket_id, worktree_id, alias),
    FOREIGN KEY (session_socket_id, worktree_id) REFERENCES worktrees(session_socket_id, worktree_id)
);
CREATE TABLE mention_group_members (
    session_socket_id TEXT NOT NULL,
    worktree_id TEXT NOT NULL,
    alias TEXT NOT NULL,
    member_order INTEGER NOT NULL,
    address_id TEXT NOT NULL,
    PRIMARY KEY (session_socket_id, worktree_id, alias, member_order),
    UNIQUE (session_socket_id, worktree_id, alias, address_id),
    FOREIGN KEY (session_socket_id, worktree_id, alias) REFERENCES mention_groups(session_socket_id, worktree_id, alias) ON DELETE CASCADE,
    FOREIGN KEY (session_socket_id, worktree_id, address_id) REFERENCES pane_addresses(session_socket_id, worktree_id, address_id)
);
CREATE TABLE terminal_executions (
    session_socket_id TEXT NOT NULL,
    worktree_id TEXT NOT NULL,
    attempt_id TEXT NOT NULL,
    terminal_id TEXT NOT NULL,
    command TEXT NOT NULL,
    started_at_ms INTEGER,
    completed_at_ms INTEGER,
    exit_code INTEGER,
    output TEXT,
    PRIMARY KEY(session_socket_id, worktree_id, attempt_id),
    FOREIGN KEY(session_socket_id, worktree_id, attempt_id)
        REFERENCES delivery_attempts(session_socket_id, worktree_id, attempt_id)
);
INSERT INTO terminal_executions VALUES('/tmp/nd-tb-1000-a7d69c9d70a7c962.sock','w1','delivery-attempt-1794160870485822029-2033385-1','term_65fc79da6f7c91','echo hello',1794160870549,1794160870566,0,unistr('stdout:\u000ahello\u000a\u000astderr:'));
CREATE TABLE coordinations (
    session_socket_id TEXT NOT NULL, worktree_id TEXT NOT NULL,
    coordination_id TEXT NOT NULL, request_id TEXT NOT NULL,
    owner_address_id TEXT NOT NULL, owner_session_id TEXT NOT NULL,
    deliverable TEXT NOT NULL, revision INTEGER NOT NULL DEFAULT 1 CHECK (revision > 0),
    outcome TEXT NOT NULL DEFAULT 'open' CHECK (outcome IN ('open','completed','blocked','cancelled')),
    report TEXT, reason TEXT, resolve_request_id TEXT, resolve_input_json TEXT,
    report_message_id TEXT, wake_revision INTEGER, wake_message_id TEXT,
    created_at_ms INTEGER NOT NULL, updated_at_ms INTEGER NOT NULL,
    PRIMARY KEY (session_socket_id, worktree_id, coordination_id),
    UNIQUE (session_socket_id, worktree_id, owner_address_id, owner_session_id, request_id),
    FOREIGN KEY (session_socket_id, worktree_id, owner_address_id)
        REFERENCES pane_addresses(session_socket_id, worktree_id, address_id)
);
CREATE TABLE coordination_assignments (
    session_socket_id TEXT NOT NULL, worktree_id TEXT NOT NULL,
    assignment_id TEXT NOT NULL, coordination_id TEXT NOT NULL,
    recipient_address_id TEXT NOT NULL, recipient_session_id TEXT NOT NULL,
    original_message_id TEXT NOT NULL, current_message_id TEXT NOT NULL,
    revision INTEGER NOT NULL CHECK (revision > 0),
    work_outcome TEXT NOT NULL CHECK (work_outcome IN ('pending','unclassified','completed','needs_continuation','blocked')),
    blocked_reason TEXT, waiting_on_assignment_id TEXT, last_reply_message_id TEXT,
    PRIMARY KEY (session_socket_id, worktree_id, assignment_id),
    FOREIGN KEY (session_socket_id, worktree_id, coordination_id)
        REFERENCES coordinations(session_socket_id, worktree_id, coordination_id),
    FOREIGN KEY (session_socket_id, worktree_id, recipient_address_id)
        REFERENCES pane_addresses(session_socket_id, worktree_id, address_id),
    FOREIGN KEY (session_socket_id, worktree_id, current_message_id)
        REFERENCES collaboration_messages(session_socket_id, worktree_id, message_id)
);
CREATE TABLE coordination_links (
    session_socket_id TEXT NOT NULL, worktree_id TEXT NOT NULL,
    message_id TEXT NOT NULL, assignment_id TEXT NOT NULL, revision INTEGER NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('request','reply')), input_json TEXT NOT NULL,
    PRIMARY KEY (session_socket_id, worktree_id, message_id, assignment_id),
    FOREIGN KEY (session_socket_id, worktree_id, message_id)
        REFERENCES collaboration_messages(session_socket_id, worktree_id, message_id),
    FOREIGN KEY (session_socket_id, worktree_id, assignment_id)
        REFERENCES coordination_assignments(session_socket_id, worktree_id, assignment_id)
);
CREATE TABLE space_envoys (
    session_socket_id TEXT NOT NULL,
    worktree_id TEXT NOT NULL,
    address_id TEXT NOT NULL,
    origin_space_id TEXT NOT NULL,
    origin_address_id TEXT NOT NULL,
    origin_alias TEXT NOT NULL,
    origin_label TEXT NOT NULL,
    host_label TEXT NOT NULL,
    PRIMARY KEY (session_socket_id, worktree_id, address_id),
    UNIQUE (session_socket_id, worktree_id, origin_space_id, origin_address_id),
    FOREIGN KEY (session_socket_id, worktree_id, address_id)
        REFERENCES pane_addresses (session_socket_id, worktree_id, address_id)
);
CREATE TABLE bridged_messages (
    session_socket_id TEXT NOT NULL,
    worktree_id TEXT NOT NULL,
    message_id TEXT NOT NULL,
    origin_space_id TEXT NOT NULL,
    origin_message_id TEXT NOT NULL,
    origin_label_snapshot TEXT NOT NULL,
    label_snapshot TEXT NOT NULL,
    PRIMARY KEY (session_socket_id, worktree_id, message_id),
    UNIQUE (session_socket_id, worktree_id, origin_space_id, origin_message_id),
    FOREIGN KEY (session_socket_id, worktree_id, message_id)
        REFERENCES collaboration_messages (session_socket_id, worktree_id, message_id)
);
CREATE TABLE bridged_threads (
    session_socket_id TEXT NOT NULL,
    worktree_id TEXT NOT NULL,
    thread_id TEXT NOT NULL,
    peer_space_id TEXT NOT NULL,
    peer_thread_id TEXT NOT NULL,
    PRIMARY KEY (session_socket_id, worktree_id, thread_id, peer_space_id),
    UNIQUE (session_socket_id, worktree_id, peer_space_id, peer_thread_id),
    FOREIGN KEY (session_socket_id, worktree_id, thread_id)
        REFERENCES collaboration_threads (session_socket_id, worktree_id, thread_id)
);
CREATE TABLE reminders (
 session_socket_id TEXT NOT NULL, worktree_id TEXT NOT NULL, reminder_id TEXT NOT NULL,
 owner_session_id TEXT NOT NULL, owner_json TEXT NOT NULL, spec_json TEXT NOT NULL,
 request_id TEXT NOT NULL, request_fingerprint TEXT NOT NULL,
 state TEXT NOT NULL CHECK(state IN ('scheduled','awaiting_delivery','completed','cancelled','failed')),
 state_json TEXT NOT NULL, due_at_ms INTEGER,
 PRIMARY KEY(session_socket_id,worktree_id,reminder_id),
 UNIQUE(session_socket_id,worktree_id,owner_session_id,request_id),
 CHECK((state='scheduled')=(due_at_ms IS NOT NULL)),
 FOREIGN KEY(session_socket_id,worktree_id) REFERENCES worktrees(session_socket_id,worktree_id)
);
CREATE TABLE reminder_fires (
 session_socket_id TEXT NOT NULL, worktree_id TEXT NOT NULL, reminder_id TEXT NOT NULL,
 scheduled_at_ms INTEGER NOT NULL, ordinal INTEGER NOT NULL CHECK(ordinal BETWEEN 1 AND 100),
 message_id TEXT NOT NULL, delivery_id TEXT NOT NULL,
 PRIMARY KEY(session_socket_id,worktree_id,reminder_id,scheduled_at_ms),
 UNIQUE(session_socket_id,worktree_id,message_id), UNIQUE(session_socket_id,worktree_id,delivery_id),
 FOREIGN KEY(session_socket_id,worktree_id,reminder_id) REFERENCES reminders(session_socket_id,worktree_id,reminder_id),
 FOREIGN KEY(session_socket_id,worktree_id,message_id) REFERENCES collaboration_messages(session_socket_id,worktree_id,message_id),
 FOREIGN KEY(session_socket_id,worktree_id,delivery_id) REFERENCES mailbox_deliveries(session_socket_id,worktree_id,delivery_id)
);
CREATE VIEW delivery_seats AS
SELECT d.session_socket_id, d.worktree_id, d.delivery_id,
       COALESCE(c.state, d.state) AS progress
FROM mailbox_deliveries d
LEFT JOIN space_envoys e
  ON e.session_socket_id = d.session_socket_id AND e.worktree_id = d.worktree_id
 AND e.address_id = d.address_id
LEFT JOIN bridged_messages b
  ON b.session_socket_id = d.session_socket_id AND b.worktree_id = e.origin_space_id
 AND b.origin_space_id = d.worktree_id AND b.origin_message_id = d.message_id
LEFT JOIN mailbox_deliveries c
  ON c.session_socket_id = b.session_socket_id AND c.worktree_id = b.worktree_id
 AND c.message_id = b.message_id AND c.address_id = e.origin_address_id;
CREATE TRIGGER immutable_acknowledgements_update
BEFORE UPDATE ON delivery_acknowledgements BEGIN
    SELECT RAISE(ABORT, 'delivery acknowledgements are immutable');
END;
CREATE TRIGGER immutable_attempts_update
BEFORE UPDATE ON delivery_attempts BEGIN
    SELECT RAISE(ABORT, 'delivery attempts are immutable');
END;
CREATE TRIGGER immutable_dispositions_update
BEFORE UPDATE ON delivery_dispositions BEGIN
    SELECT RAISE(ABORT, 'delivery dispositions are immutable');
END;
CREATE TRIGGER immutable_messages_update
BEFORE UPDATE ON collaboration_messages BEGIN
    SELECT RAISE(ABORT, 'collaboration messages are immutable');
END;
CREATE TRIGGER immutable_recipients_update
BEFORE UPDATE ON message_recipients BEGIN
    SELECT RAISE(ABORT, 'message recipients are immutable');
END;
CREATE TRIGGER immutable_references_update
BEFORE UPDATE ON context_references BEGIN
    SELECT RAISE(ABORT, 'context references are immutable');
END;
CREATE TRIGGER immutable_thread_snapshots_update
BEFORE UPDATE ON thread_context_snapshots BEGIN
    SELECT RAISE(ABORT, 'thread context snapshots are immutable');
END;
CREATE TRIGGER immutable_transitions_update
BEFORE UPDATE ON transition_facts BEGIN
    SELECT RAISE(ABORT, 'transition facts are immutable');
END;
CREATE TRIGGER immutable_working_snapshots_update
BEFORE UPDATE ON working_context_snapshots BEGIN
    SELECT RAISE(ABORT, 'working context snapshots are immutable');
END;
CREATE TRIGGER group_alias_conflict BEFORE INSERT ON mention_groups
WHEN EXISTS (SELECT 1 FROM alias_claims WHERE session_socket_id=NEW.session_socket_id AND worktree_id=NEW.worktree_id AND alias=NEW.alias)
BEGIN SELECT RAISE(ABORT, 'group alias is already reserved by an agent'); END;
CREATE TRIGGER agent_alias_group_conflict BEFORE INSERT ON alias_claims
WHEN EXISTS (SELECT 1 FROM mention_groups WHERE session_socket_id=NEW.session_socket_id AND worktree_id=NEW.worktree_id AND alias=NEW.alias)
BEGIN SELECT RAISE(ABORT, 'agent alias is already reserved by a group'); END;
CREATE TRIGGER terminal_command_immutable BEFORE UPDATE OF terminal_id,command ON terminal_executions
BEGIN SELECT RAISE(ABORT,'terminal command identity is immutable'); END;
CREATE TRIGGER terminal_result_immutable BEFORE UPDATE OF completed_at_ms,exit_code,output ON terminal_executions
WHEN OLD.completed_at_ms IS NOT NULL
BEGIN SELECT RAISE(ABORT,'terminal result is immutable'); END;
CREATE UNIQUE INDEX one_active_alias_per_address
    ON alias_claims (session_socket_id, worktree_id, address_id)
    WHERE claim_state = 'active';
CREATE UNIQUE INDEX one_active_attempt_per_address
    ON delivery_attempt_state (session_socket_id, worktree_id, address_id)
    WHERE active = 1;
CREATE UNIQUE INDEX one_active_binding_per_address
    ON pane_bindings (session_socket_id, worktree_id, address_id) WHERE active = 1;
CREATE UNIQUE INDEX one_active_binding_per_pane
    ON pane_bindings (session_socket_id, worktree_id, pane_id) WHERE active = 1;
CREATE UNIQUE INDEX one_attention_item_per_unresolved_source
    ON attention_items (session_socket_id, worktree_id, source_kind, source_id)
    WHERE resolved_at_ms IS NULL;
CREATE INDEX coordination_assignment_parent ON coordination_assignments(session_socket_id, worktree_id, coordination_id);
CREATE INDEX reminders_pending ON reminders(due_at_ms) WHERE state IN ('scheduled','awaiting_delivery');
CREATE INDEX reminders_owner ON reminders(session_socket_id,worktree_id,owner_session_id,state);
CREATE INDEX mailbox_deliveries_message
              ON mailbox_deliveries(session_socket_id, worktree_id, message_id);
CREATE INDEX transition_facts_attention_acknowledgement
              ON transition_facts(session_socket_id, worktree_id, entity_id)
              WHERE entity_kind='attention_item' AND transition_kind='acknowledged';
CREATE INDEX coordination_links_assignment
              ON coordination_links(session_socket_id, worktree_id, assignment_id, message_id);
CREATE INDEX transition_facts_entity
              ON transition_facts(session_socket_id, worktree_id, entity_kind, entity_id,
                                  transition_kind, occurred_at_ms);
CREATE INDEX agent_session_observations_status
              ON agent_session_observations(session_socket_id, worktree_id, address_id,
                                            agent_session_id, status, observed_at_ms);
COMMIT;
PRAGMA user_version=7;

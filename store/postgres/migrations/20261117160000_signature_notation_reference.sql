ALTER TABLE signature ADD CONSTRAINT signature_notation_id_fkey FOREIGN KEY (notation_id) REFERENCES notation(id) NOT VALID;

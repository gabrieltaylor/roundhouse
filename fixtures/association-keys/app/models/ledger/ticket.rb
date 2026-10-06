module Ledger
  class Ticket < ApplicationRecord
    self.table_name = "case_tickets"
    belongs_to :account
  end
end
